import Foundation
import CoreGraphics

struct TextReadPolicy: Decodable, Sendable {
    let kind: String
    let window_id: String
    func valid(bundle: String?, pid: UInt32?) -> Bool {
        kind == "registered_editor" && pid.map { $0 > 0 } == true &&
            ["com.lingxi.computer-user.guard-fixture", "com.lingxi.computer-user.mvp-fixture", "com.apple.TextEdit"].contains(bundle ?? "") &&
            window_id.hasPrefix("window-") && UInt32(window_id.dropFirst(7)).map { $0 > 0 } == true
    }
}

struct NativeBackgroundProcessIdentity: Decodable, Sendable, Equatable {
    let pid: UInt32
    let start_seconds: UInt64
    let start_micros: UInt64
}

struct NativeBackgroundTarget: Decodable, Sendable {
    let window_id: String
    let process: NativeBackgroundProcessIdentity?

    func valid(bundle: String?, pid: UInt32?) -> Bool {
        guard let bundle, !bundle.isEmpty, let pid, pid > 0,
              window_id.hasPrefix("window-"),
              let suffix = UInt32(window_id.dropFirst("window-".count)),
              suffix > 0, String(suffix) == String(window_id.dropFirst("window-".count)) else { return false }
        if let process {
            guard process.pid == pid, process.pid <= UInt32(Int32.max),
                  process.start_seconds > 0, process.start_micros < 1_000_000 else { return false }
        }
        return true
    }
}

enum ObservationFact<Value: Encodable & Sendable>: Encodable, Sendable {
    case known(Value)
    case absent
    case unavailable

    private enum CodingKeys: String, CodingKey { case state, value }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .known(let value):
            try container.encode("known", forKey: .state)
            try container.encode(value, forKey: .value)
        case .absent:
            try container.encode("absent", forKey: .state)
        case .unavailable:
            try container.encode("unavailable", forKey: .state)
        }
    }
}

struct GlobalFocusObservation: Encodable, Sendable {
    var app: ObservationFact<AppIdentity> = .unavailable
    var window_id: ObservationFact<String> = .unavailable
}

func reconcileGlobalFocusObservation(read: GlobalFocusObservation,
                                     current: GlobalFocusObservation) -> GlobalFocusObservation {
    guard case .known(let readApp) = read.app,
          case .known(let readWindow) = read.window_id,
          case .known(let currentApp) = current.app,
          readApp.bundle_id == currentApp.bundle_id,
          readApp.pid == currentApp.pid else { return current }
    return GlobalFocusObservation(app: .known(currentApp), window_id: .known(readWindow))
}

struct NativeBackgroundWindowContext: Encodable, Sendable {
    var window: WindowContext
    var hidden: ObservationFact<Bool>
    var minimized: ObservationFact<Bool>
}

struct SurfaceRef: Codable, Sendable, Equatable {
    let id: String
    let epoch: UInt64

    var valid: Bool {
        !id.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && id.utf8.count <= 256
            && !id.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
            && epoch > 0
    }
}

struct NativeWatchScope: Sendable, Equatable {
    let watchID: String
    let sessionEpoch: UInt64
    let surface: SurfaceRef

    var valid: Bool {
        !watchID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && watchID.utf8.count <= 256
            && !watchID.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
            && sessionEpoch > 0
            && surface.valid
    }
}

struct NativeRequest: Decodable, Sendable {
    let protocol_version: Int
    let command: String
    let bundle_id: String?
    let pid: UInt32?
    let output_dir: String?
    let timeout_ms: Int?
    let max_nodes: Int?
    let max_depth: Int?
    let max_ocr_blocks: Int?
    let interval_ms: Int?
    let clock_origin_unix_ms: UInt64?
    let text_policy: TextReadPolicy?
    let table_scope: TableScopeSearchRequest?
    let background_target: NativeBackgroundTarget?
    let session_epoch: UInt64?
    let watch_id: String?
    let surface: SurfaceRef?

    var timeout: Int { min(max(timeout_ms ?? 1500, 100), 5000) }
    var nodeLimit: Int { min(max(max_nodes ?? 512, 1), 512) }
    var depthLimit: Int { min(max(max_depth ?? 12, 1), 12) }
    var ocrLimit: Int { min(max(max_ocr_blocks ?? 128, 1), 128) }
    var interval: Int { min(max(interval_ms ?? 1000, 250), 5000) }

    var watchScope: NativeWatchScope? {
        guard command == "watch", let watch_id, let session_epoch, let surface,
              let clock_origin_unix_ms, clock_origin_unix_ms > 0 else { return nil }
        let scope = NativeWatchScope(watchID: watch_id, sessionEpoch: session_epoch, surface: surface)
        return scope.valid ? scope : nil
    }

    func validBackgroundTarget() -> Bool {
        guard let background_target else { return true }
        return command == "watch" && watchScope != nil && table_scope == nil
            && text_policy == nil && background_target.valid(bundle: bundle_id, pid: pid)
    }
}

struct PermissionState: Codable, Sendable {
    let accessibility: Bool
    let screen_capture: Bool
}

struct PermissionsResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "permissions"
    let accessibility: Bool
    let screen_capture: Bool
}

struct ErrorResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "error"
    let error: String
}

/// A watch has already emitted its scoped terminal record. Main exits nonzero
/// without appending an unscoped ErrorResponse to the watch NDJSON stream.
struct WatchTerminalFailure: Error, Sendable {}

func shouldEmitGenericErrorResponse(for error: Error) -> Bool {
    !(error is WatchTerminalFailure)
}

struct Rect: Codable, Sendable, Equatable {
    let x: Double
    let y: Double
    let width: Double
    let height: Double

    init(_ rect: CGRect) {
        x = rect.origin.x; y = rect.origin.y
        width = rect.width; height = rect.height
    }

    var cgRect: CGRect { CGRect(x: x, y: y, width: width, height: height) }
    var valid: Bool {
        [x, y, width, height].allSatisfy(\.isFinite) && width >= 0 && height >= 0
    }
    func intersects(_ other: Rect) -> Bool {
        width > 0 && height > 0 && other.width > 0 && other.height > 0 &&
        x < other.x + other.width && other.x < x + width &&
        y < other.y + other.height && other.y < y + height
    }
    func approximatelyEquals(_ other: Rect) -> Bool {
        zip([x, y, width, height], [other.x, other.y, other.width, other.height])
            .allSatisfy { abs($0 - $1) < 0.5 }
    }
}

enum UiValue: Encodable, Sendable, Equatable {
    case bool(Bool), text(String), number(Double)

    private enum CodingKeys: String, CodingKey { case type, value }
    func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .bool(let v): try c.encode("bool", forKey: .type); try c.encode(v, forKey: .value)
        case .text(let v): try c.encode("text", forKey: .type); try c.encode(v, forKey: .value)
        case .number(let v): try c.encode("number", forKey: .type); try c.encode(v, forKey: .value)
        }
    }
}

/// Source-owned AX scrollbar units. Bounds are reported by the application;
/// absent or invalid bounds never become an inferred scroll boundary.
struct ScrollAxisState: Encodable, Sendable, Equatable {
    let value: Double
    let min: Double?
    let max: Double?

    init?(value: Double, min: Double?, max: Double?) {
        guard value.isFinite else { return nil }
        if let min { guard min.isFinite, value >= min else { return nil } }
        if let max { guard max.isFinite, value <= max else { return nil } }
        if let min, let max { guard min < max else { return nil } }
        self.value = value; self.min = min; self.max = max
    }
}

struct ScrollState: Encodable, Sendable, Equatable {
    let horizontal: ScrollAxisState?
    let vertical: ScrollAxisState?
}

enum UiObjectIdentityContinuity: String, Encodable, Sendable, Equatable {
    case notificationCovered = "notification_covered"
    case readScoped = "read_scoped"
}

enum ObserverCoverageState: String, Encodable, Sendable, Equatable {
    case available, degraded, unavailable
}

enum AxEnabledObservation: String, Encodable, Sendable, Equatable {
    case enabled
    case disabled
    case attributeUnsupported = "attribute_unsupported"
    case unavailable
}

enum ObserverNotification: String, Encodable, Sendable, Equatable, CaseIterable {
    case uiElementDestroyed = "ui_element_destroyed"
    case created
    case moved
    case resized
    case layoutChanged = "layout_changed"
    case valueChanged = "value_changed"
    case titleChanged = "title_changed"
    case rowCountChanged = "row_count_changed"
}

struct ObserverRegistrationFailure: Encodable, Sendable, Equatable {
    let instance_ref: String
    let notification: ObserverNotification
    let error_code: Int32
}

struct ObserverCoverage: Encodable, Sendable, Equatable {
    let state: ObserverCoverageState
    let covered_objects: UInt32
    let read_scoped_objects: UInt32
    let failed_registrations: UInt32
    let failures: [ObserverRegistrationFailure]

    static let unavailable = ObserverCoverage(state: .unavailable, covered_objects: 0,
        read_scoped_objects: 0, failed_registrations: 0, failures: [])

    static func project(objects: [UiObject], rootAvailable: Bool,
                        failures: [ObserverRegistrationFailure]) -> ObserverCoverage {
        let objectModes = Dictionary(uniqueKeysWithValues: objects.map { ($0.instance_ref, $0.identity_continuity) })
        var seen = Set<String>()
        let finalFailures = failures.filter { failure in
            guard failure.error_code != 0,
                  objectModes[failure.instance_ref] == .readScoped else { return false }
            return seen.insert("\(failure.instance_ref)\u{0}\(failure.notification.rawValue)").inserted
        }
        let orderedFailures = finalFailures.sorted { left, right in
            let leftIndex = objects.firstIndex { $0.instance_ref == left.instance_ref } ?? objects.endIndex
            let rightIndex = objects.firstIndex { $0.instance_ref == right.instance_ref } ?? objects.endIndex
            if leftIndex != rightIndex { return leftIndex < rightIndex }
            return left.notification.rawValue < right.notification.rawValue
        }
        let covered = objects.reduce(into: UInt32(0)) { count, object in
            if object.identity_continuity == .notificationCovered { count += 1 }
        }
        let readScoped = UInt32(objects.count) - covered
        let state: ObserverCoverageState
        if !rootAvailable {
            state = .unavailable
        } else if readScoped > 0 || !orderedFailures.isEmpty {
            state = .degraded
        } else {
            state = .available
        }
        return ObserverCoverage(state: state, covered_objects: covered,
            read_scoped_objects: readScoped,
            failed_registrations: UInt32(orderedFailures.count),
            failures: Array(orderedFailures.prefix(16)))
    }
}

struct AXIdentityAssignment {
    let instance_ref: String
    let identity_continuity: UiObjectIdentityContinuity
}

struct UiObject: Encodable, Sendable {
    let instance_ref: String
    let parent: String?
    let window_id: String
    let role: String
    let subrole: String?
    let title: String?
    let description: String?
    let value: UiValue?
    let enabled: Bool
    let focused: Bool
    let frame: Rect
    let actions: [String]
    /// Present only when background AX Source copied the complete bounded
    /// AXActionNames list for this actual element.
    var ax_action_names: [String]? = nil
    var in_view: Bool
    let group: String?
    let secure: Bool
    var identity_continuity: UiObjectIdentityContinuity = .readScoped
    var scroll: ScrollState? = nil
}

struct WindowInfo: Encodable, Sendable {
    let window_id: String
    let title: String?
    let frame: Rect
    let z_index: UInt32
    let kind: String
    let modal: Bool
    let owner_bundle_id: String
}

struct AppIdentity: Encodable, Sendable {
    let bundle_id: String
    let name: String
    let pid: UInt32
}

struct SourceIdentityBoundary: Encodable, Sendable, Equatable {
    let namespace: String
    let generation: UInt64
}

struct Structure: Encodable, Sendable {
    let app: AppIdentity
    var global_focus = GlobalFocusObservation()
    var identity_boundary: SourceIdentityBoundary? = nil
    var windows: [WindowInfo] = []
    var active_layer: String?
    var complete = false
    var incomplete_reason: String? = "not observed"
    var objects: [UiObject] = []
    var observer_coverage = ObserverCoverage.unavailable
    /// Actual AXVisibleRows projection; its local completeness never upgrades
    /// the whole-tree `complete` flag above.
    var table_viewports: [TableViewport] = []
    /// A scoped census is independent evidence and never upgrades whole-tree completeness.
    var table_scope_search: TableScopeSearchResult?
    var menu_anchors: [MenuOpenerAnchor] = []
    var menu_context: MenuContext?
    var focus_context: FocusContext?
    var window_context: WindowContext?
    /// Fixed-target background AX facts; never an active-layer or focus claim.
    var background_window_context: NativeBackgroundWindowContext?
    /// Independently verified private-document scope; generic completeness is unchanged.
    var owned_document_evidence: OwnedDocumentReceiverEvidence?

    private enum CodingKeys: String, CodingKey {
        case app, global_focus, identity_boundary, windows, active_layer, complete, incomplete_reason, objects
        case observer_coverage, table_viewports, table_scope_search, menu_anchors, menu_context
        case focus_context, window_context, background_window_context, owned_document_evidence
    }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(app, forKey: .app)
        try values.encode(global_focus, forKey: .global_focus)
        try values.encodeIfPresent(identity_boundary, forKey: .identity_boundary)
        try values.encode(windows, forKey: .windows)
        try values.encodeIfPresent(active_layer, forKey: .active_layer)
        try values.encode(complete, forKey: .complete)
        try values.encodeIfPresent(incomplete_reason, forKey: .incomplete_reason)
        try values.encode(objects, forKey: .objects)
        try values.encode(observer_coverage, forKey: .observer_coverage)
        try values.encode(table_viewports, forKey: .table_viewports)
        try values.encodeIfPresent(table_scope_search, forKey: .table_scope_search)
        try values.encode(menu_anchors, forKey: .menu_anchors)
        try values.encodeIfPresent(menu_context, forKey: .menu_context)
        try values.encodeIfPresent(focus_context, forKey: .focus_context)
        try values.encodeIfPresent(window_context, forKey: .window_context)
        try values.encodeIfPresent(background_window_context, forKey: .background_window_context)
        try values.encodeIfPresent(owned_document_evidence, forKey: .owned_document_evidence)
    }

    mutating func normalizeSourceProvenance(sequence: UInt64, observedMS: UInt64) {
        for index in menu_anchors.indices {
            menu_anchors[index].verified_source_sequence = sequence
            menu_anchors[index].verified_observed_ms = observedMS
        }
        if var focus = focus_context {
            focus.verified_source_sequence = sequence
            focus.verified_observed_ms = observedMS
            focus.composition.normalizeProvenance(sequence: sequence, observedMS: observedMS)
            focus_context = focus
        }
        if var window = window_context {
            window.verified_source_sequence = sequence
            window.verified_observed_ms = observedMS
            window_context = window
        }
        if var background = background_window_context {
            background.window.verified_source_sequence = sequence
            background.window.verified_observed_ms = observedMS
            background_window_context = background
        }
        if var context = menu_context {
            context.opener.verified_source_sequence = sequence
            context.opener.verified_observed_ms = observedMS
            menu_context = context
        }
    }
}

struct Frame: Encodable, Sendable {
    let window_id: String
    let pid: UInt32
    let frame: Rect
    let width_px: Int
    let height_px: Int
    let scale_factor: Double
    let started_uptime_ns: UInt64
    let finished_uptime_ns: UInt64
    let png_file: String
    let byte_length: Int
}

struct OcrBlock: Encodable, Sendable {
    let text: String
    let confidence: Float
    let frame: Rect
}

struct Ocr: Encodable, Sendable {
    var complete = false
    var reason: String? = "not captured"
    var blocks: [OcrBlock] = []
}

struct ObservationResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "observation"
    let permissions: PermissionState
    var structure: Structure
    var structure_started_uptime_ns: UInt64
    var structure_finished_uptime_ns: UInt64
    var frame: Frame?
    var ocr = Ocr()
    var errors: [String] = []
}

struct StreamFrame: Encodable, Sendable {
    let status: String
    let display_time_ticks: UInt64?
    let display_time_uptime_ns: UInt64?
    let source_unix_ms: UInt64?
    let geometry_revision: UInt64
    let width_px: Int
    let height_px: Int
    let scale_factor: Double
    let content_scale: Double
    let content_rect: Rect
    let dirty_rects: [Rect]
    let frame_id: String?
    let window_id: String
    let pid: UInt32
    let frame: Rect
    let byte_length: Int
    let png_file: String?
}

struct StreamFrameResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "frame"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let frame: StreamFrame
}

struct StreamObservationResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "observation"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let permissions: PermissionState
    var structure: Structure
    let structure_started_uptime_ns: UInt64
    let structure_finished_uptime_ns: UInt64
    let structure_started_unix_ms: UInt64
    let structure_finished_unix_ms: UInt64
    let frame_ref: String?
    let tree_revision: UInt64
    let observer_revision: UInt64
    let observer_available: Bool
    let observer_reason: String?
    let ocr: Ocr
    let errors: [String]
}

struct CaptureFrame: Encodable, Sendable {
    let geometry_revision: UInt64
    let width_px: Int
    let height_px: Int
    let scale_factor: Double
    let content_scale: Double
    let content_rect: Rect
    let window_id: String
    let pid: UInt32
    let frame: Rect
    let byte_length: Int
    let png_file: String
    /// A record identifier for this JSON frame event. PNG identity is carried
    /// separately by CaptureResultResponse.image_frame_id.
    let frame_id: String
}

struct CaptureResultResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "capture_result"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let watch_id: String
    let request_id: String
    let session_epoch: UInt64
    let surface: SurfaceRef
    let status: String
    let requested_after_ms: UInt64
    let deadline_ms: UInt64
    let capture_started_uptime_ns: UInt64?
    let capture_finished_uptime_ns: UInt64?
    let capture_started_unix_ms: UInt64?
    let capture_finished_unix_ms: UInt64?
    let image_frame_id: String?
    let frame: CaptureFrame?
    let observation: StreamObservationResponse?
    let code: String?
}

struct StreamHealthResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "health"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let source: String?
    let state: String
    let reason: String?
}

/// A terminal watch health record retains the same validated watch identity as
/// its stream header so the Source reader can attribute a failed session.
struct StreamTerminalResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "health"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let source: String
    let state: String
    let reason: String
    let session_epoch: UInt64
    let surface: SurfaceRef
}

struct StreamGapResponse: Encodable, Sendable {
    let protocol_version = 1
    let kind = "gap"
    let stream_id: String
    let sequence: UInt64
    let received_uptime_ns: UInt64
    let received_unix_ms: UInt64
    let dropped_frames: UInt64
    let reason: String
}

func uptimeNs() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
func unixMs() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

func validBundleID(_ bundle: String) -> Bool {
    !bundle.isEmpty && bundle.utf8.count <= 256 && bundle.utf8.allSatisfy {
        (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || [45, 46, 95].contains($0)
    }
}

/// Keep all model-visible strings and the resulting IPC record bounded.
func boundedText(_ text: String, bytes: Int = 512) -> String {
    var result = String(decoding: text.utf8.prefix(bytes), as: UTF8.self)
        .filter { !$0.isControlCharacter || $0 == "\n" || $0 == "\t" }
    while result.utf8.count > bytes { result.removeLast() }
    return result
}

private extension Character {
    var isControlCharacter: Bool { unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) } }
}
