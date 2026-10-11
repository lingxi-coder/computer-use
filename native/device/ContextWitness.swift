import Foundation
import ApplicationServices
import AppKit
import CryptoKit

/// Value-only WholeStructure witness emitted by the shared contracts. These
/// values are current semantic facts; they contain no Source ObjectRefs and
/// make no claim that a Device AX proxy is the original Source proxy.
struct NativeDeviceContextApp: Codable, Sendable, Equatable {
    let bundle_id: String
    let pid: UInt32
}

enum NativeDeviceContextLayerKind: String, Codable, Sendable, Equatable {
    case window, sheet, dialog, popover, menu
}

struct NativeDeviceContextWindow: Codable, Sendable, Equatable {
    let window_id: String
    let title: String?
    let frame: HitRect
    let kind: NativeDeviceContextLayerKind
    let modal: Bool
    let owner_bundle_id: String

    private enum CodingKeys: String, CodingKey { case window_id, title, frame, kind, modal, owner_bundle_id }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(window_id, forKey: .window_id)
        try values.encode(title, forKey: .title)
        try values.encode(frame, forKey: .frame)
        try values.encode(kind, forKey: .kind)
        try values.encode(modal, forKey: .modal)
        try values.encode(owner_bundle_id, forKey: .owner_bundle_id)
    }
}

/// Frozen, value-only copy of the public row table scope. It carries no AX
/// reference, selector condition, or row index into the Device process.
struct NativeDeviceContextRowContainerScope: Codable, Sendable, Equatable {
    let role: String
    let label: String

    var valid: Bool {
        role == "ax_scroll_area" && !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && label.utf8.count <= 256
            && !label.unicodeScalars.contains { $0.properties.generalCategory == .control }
    }
}

struct NativeDeviceContextRowTableScope: Codable, Sendable, Equatable {
    let scroll_path: [NativeDeviceContextRowContainerScope]
    let column_headers: [String]

    private enum CodingKeys: String, CodingKey { case scroll_path, column_headers }

    var valid: Bool {
        guard !scroll_path.isEmpty, scroll_path.count <= 8,
              scroll_path.allSatisfy(\.valid),
              !column_headers.isEmpty, column_headers.count <= 64 else { return false }
        var seen = Set<String>()
        for header in column_headers {
            guard !header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  header.utf8.count <= 256,
                  !header.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return false }
            let key = bindingLabel(header)
            guard seen.insert(key).inserted else { return false }
        }
        return true
    }
}

/// The Source-owned provenance attached to a scoped normal-window root.
/// Device compares the process/window against live facts; session and sequence
/// remain provenance, not Device-side AX identity.
struct NativeDeviceSourceWindowContext: Codable, Sendable, Equatable {
    let source_session_id: String
    let session_epoch: UInt64
    let process: NativeMenuProcessIdentity
    let origin_window_id: String
    let origin_window_frame: HitRect
    let verified_source_sequence: UInt64
    let verified_observed_ms: UInt64

    private enum CodingKeys: String, CodingKey {
        case source_session_id, session_epoch, process, origin_window_id
        case origin_window_frame, verified_source_sequence, verified_observed_ms
    }

    var valid: Bool {
        nativeID(source_session_id) && session_epoch > 0 && process.valid
            && nativeID(origin_window_id) && origin_window_frame.valid
            && verified_source_sequence > 0
    }
}

struct NativeDeviceContextWindowRootV1: Codable, Sendable, Equatable {
    let app: NativeDeviceContextApp
    let window: NativeDeviceContextWindow
    let root: NativeDeviceContextObject
    let source: NativeDeviceSourceWindowContext

    private enum CodingKeys: String, CodingKey { case app, window, root, source }

    var valid: Bool {
        app.pid > 0 && app.pid <= UInt32(Int32.max) && nativeID(app.bundle_id)
            && nativeID(window.window_id) && window.window_id.hasPrefix("window-")
            && UInt32(window.window_id.dropFirst(7)).map({ $0 > 0 }) == true
            && window.kind == .window && !window.modal
            && window.owner_bundle_id == app.bundle_id && window.frame.valid
            && (window.title.map({ $0.utf8.count <= 512
                && !$0.unicodeScalars.contains { $0.properties.generalCategory == .control }
                && bindingLabel($0) == $0 }) ?? false)
            && root.role == kAXWindowRole && root.subrole == kAXStandardWindowSubrole
            && root.label == bindingLabel(window.title ?? "") && root.label.utf8.count <= 512
            && root.frame == window.frame
            && source.valid && source.process.pid == app.pid
            && source.origin_window_id == window.window_id
            && source.origin_window_frame == window.frame
    }

    func matchesSourceProvenance(sequence: UInt64, observedMS: UInt64) -> Bool {
        valid && source.verified_source_sequence == sequence
            && source.verified_observed_ms == observedMS
    }
}

/// Source's tri-state visibility fact for a background window. Unknown and
/// absent remain distinct and never become permission to submit.
enum NativeDeviceBooleanObservationFact: Codable, Sendable, Equatable {
    case known(Bool)
    case absent
    case unavailable

    private enum CodingKeys: String, CodingKey { case state, value }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .state) {
        case "known": self = .known(try values.decode(Bool.self, forKey: .value))
        case "absent": self = .absent
        case "unavailable": self = .unavailable
        default:
            throw DecodingError.dataCorruptedError(forKey: .state, in: values,
                debugDescription: "unknown Source boolean observation state")
        }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .known(let value):
            try values.encode("known", forKey: .state)
            try values.encode(value, forKey: .value)
        case .absent:
            try values.encode("absent", forKey: .state)
        case .unavailable:
            try values.encode("unavailable", forKey: .state)
        }
    }

    var isKnownFalse: Bool {
        if case .known(false) = self { return true }
        return false
    }
}

/// Exact Source-owned background-window provenance. The target context hash
/// binds the source session, kernel birth identity, window ID, and geometry.
struct NativeDeviceSourceBackgroundWindowContext: Codable, Sendable, Equatable {
    let window: NativeDeviceSourceWindowContext
    let hidden: NativeDeviceBooleanObservationFact
    let minimized: NativeDeviceBooleanObservationFact
    let target_context_hash: String

    private enum CodingKeys: String, CodingKey { case window, hidden, minimized, target_context_hash }

    var validHashShape: Bool {
        let bytes = Array(target_context_hash.utf8)
        return bytes.count == 68 && Array(bytes.prefix(4)) == Array("tc1:".utf8)
            && bytes.dropFirst(4).allSatisfy {
                ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102)
            }
    }

    func valid(bundleID: String, pid: UInt32, windowID: String, frame: HitRect) -> Bool {
        window.valid && validHashShape && nativeID(bundleID) && pid > 0
            && window.process.pid == pid && window.origin_window_id == windowID
            && window.origin_window_frame == frame && frame.valid
            && target_context_hash == computedTargetContextHash(bundleID: bundleID, pid: pid)
    }

    func matchesSourceProvenance(sequence: UInt64, observedMS: UInt64) -> Bool {
        window.verified_source_sequence == sequence && window.verified_observed_ms == observedMS
    }

    func computedTargetContextHash(bundleID: String, pid: UInt32) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        func jsonString(_ value: String) -> String {
            String(decoding: try! encoder.encode(value), as: UTF8.self)
        }
        func rect(_ value: HitRect) -> String {
            // Encode normalized IEEE-754 bits so Swift and Rust hash identical frame bytes.
            func encodedBits(_ number: Double) -> String {
                let bits = String(format: "%016llx", number == 0 ? UInt64(0) : number.bitPattern)
                return jsonString(bits)
            }
            return "{\"x\":\(encodedBits(value.x)),\"y\":\(encodedBits(value.y)),"
                + "\"width\":\(encodedBits(value.width)),\"height\":\(encodedBits(value.height))}"
        }
        let process = window.process
        let canonical = "{\"version\":1,\"kind\":\"native_window\","
            + "\"bundle_id\":\(jsonString(bundleID)),\"pid\":\(pid),"
            + "\"process\":{\"pid\":\(process.pid),\"start_seconds\":\(process.start_seconds),\"start_micros\":\(process.start_micros)},"
            + "\"source_session_id\":\(jsonString(window.source_session_id)),"
            + "\"session_epoch\":\(window.session_epoch),\"window_id\":\(jsonString(window.origin_window_id)),"
            + "\"window_frame\":\(rect(window.origin_window_frame))}"
        let digest = SHA256.hash(data: Data(canonical.utf8))
            .map { String(format: "%02x", $0) }.joined()
        return "tc1:" + digest
    }
}

/// Facts copied from the Device's current AX root read. This is intentionally
/// value-only; same-worker CF continuity is checked by the caller retaining
/// the selected AXUIElement and re-enumerating AXWindows.
struct NativeDeviceContextWindowRootFacts: Equatable {
    let app: NativeDeviceContextApp
    let process: NativeMenuProcessIdentity
    let window: NativeDeviceContextWindow
    let root: NativeDeviceContextObject

    var valid: Bool {
        app.pid > 0 && process.valid && process.pid == app.pid
            && nativeID(app.bundle_id) && window.kind == .window && !window.modal
            && window.owner_bundle_id == app.bundle_id && window.frame.valid
            && window.title.map({ $0.utf8.count <= 512 && bindingLabel($0) == $0 }) == true
            && root.role == kAXWindowRole && root.subrole == kAXStandardWindowSubrole
            && root.label == bindingLabel(window.title ?? "") && root.frame == window.frame
    }
}

func strictNativeRowTableScopeFields(_ raw: Any?) -> Bool {
    guard let value = raw as? [String: Any], Set(value.keys) == ["scroll_path", "column_headers"],
          let path = value["scroll_path"] as? [[String: Any]], !path.isEmpty, path.count <= 8,
          let headers = value["column_headers"] as? [String], !headers.isEmpty, headers.count <= 64 else { return false }
    guard path.allSatisfy({
        Set($0.keys) == ["role", "label"] && ($0["role"] as? String) == "ax_scroll_area"
            && (($0["label"] as? String).map { label in
                !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && label.utf8.count <= 256
                    && !label.unicodeScalars.contains { $0.properties.generalCategory == .control }
            } ?? false)
    }) else { return false }
    var seen = Set<String>()
    return headers.allSatisfy { header in
        guard !header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              header.utf8.count <= 256,
              !header.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return false }
        return seen.insert(bindingLabel(header)).inserted
    }
}

func nativeWindowRootWitnessMatches(_ expected: NativeDeviceContextWindowRootV1,
                                    actual: NativeDeviceContextWindowRootFacts,
                                    binding: NativeBindingFingerprint,
                                    displayFrame: HitRect) -> Bool {
    expected.valid && actual.valid && actual.app == expected.app
        && actual.process == expected.source.process
        && actual.window.window_id == expected.window.window_id
        && actual.window.title == expected.window.title
        && actual.window.kind == .window && !actual.window.modal
        && actual.window.owner_bundle_id == actual.app.bundle_id
        && expected.window.frame.matches(actual.window.frame)
        && actual.root.role == kAXWindowRole && actual.root.subrole == kAXStandardWindowSubrole
        && actual.root.label == expected.root.label
        && actual.root.frame.matches(actual.window.frame)
        && actual.root.frame.matches(displayFrame)
        && binding.app == actual.app.bundle_id && binding.app_pid == actual.app.pid
        && binding.window_id == actual.window.window_id && binding.ancestor_roles.isEmpty
        && binding.group == nil && binding.role == kAXWindowRole
        && binding.subrole == kAXStandardWindowSubrole && binding.label == actual.root.label
        && binding.sibling_index == 0 && binding.scroll_container == nil
        && binding.frame == actual.root.frame.rounded && displayFrame.valid
    }

/// A dedicated bounded read for activation-only root proofs. It deliberately
/// avoids `readContext`, whose complete ancestry and sibling closure belongs
/// to whole-tree input. CF membership and continuity stay with the caller.
extension NativeBindingReader {
    func readWindowRoot(_ expected: NativeDeviceContextWindowRootV1,
                        binding: NativeBindingFingerprint,
                        displayFrame: HitRect) -> NativeDeviceContextWindowRootFacts? {
        guard let initialStamp = DeviceProcessStamp.read(pid),
              initialStamp == expected.source.process.stamp else { return nil }
        guard expected.valid, expected.app.bundle_id == bundle,
              expected.app.pid == UInt32(pid), expected.window.window_id == "window-\(windowID)",
              monotonicMS() < deadline else { return nil }

        func copy(_ key: String) -> CFTypeRef? {
            let now = monotonicMS()
            guard now < deadline,
                  AXUIElementSetMessagingTimeout(window, Float(min(25, deadline - now)) / 1000) == .success else { return nil }
            var value: CFTypeRef?
            guard AXUIElementCopyAttributeValue(window, key as CFString, &value) == .success,
                  let value, monotonicMS() < deadline else { return nil }
            return value
        }

        func actualFrame() -> HitRect? {
            guard let position = copy(kAXPositionAttribute), CFGetTypeID(position) == AXValueGetTypeID(),
                  let size = copy(kAXSizeAttribute), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
            var point = CGPoint.zero, dimensions = CGSize.zero
            guard AXValueGetValue(position as! AXValue, .cgPoint, &point),
                  AXValueGetValue(size as! AXValue, .cgSize, &dimensions) else { return nil }
            let result = HitRect(CGRect(origin: point, size: dimensions))
            return result.valid ? result : nil
        }

        var owner: pid_t = 0
        let timeoutNow = monotonicMS()
        guard timeoutNow < deadline,
              AXUIElementSetMessagingTimeout(window, Float(min(25, deadline - timeoutNow)) / 1000) == .success,
              AXUIElementGetPid(window, &owner) == .success, owner == pid,
              monotonicMS() < deadline,
              copy(kAXRoleAttribute) as? String == kAXWindowRole,
              copy(kAXSubroleAttribute) as? String == kAXStandardWindowSubrole,
              let title = copy(kAXTitleAttribute) as? String,
              title.utf8.count <= 512,
              !title.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }),
              let modalValue = copy(kAXModalAttribute), CFGetTypeID(modalValue) == CFBooleanGetTypeID(),
              let modal = modalValue as? NSNumber, !modal.boolValue,
              let frame = actualFrame(),
              let stamp = DeviceProcessStamp.read(pid), stamp == expected.source.process.stamp,
              monotonicMS() < deadline,
              AXUIElementGetPid(window, &owner) == .success, owner == pid,
              monotonicMS() < deadline else { return nil }

        let app = NativeDeviceContextApp(bundle_id: bundle, pid: UInt32(pid))
        let windowFacts = NativeDeviceContextWindow(window_id: "window-\(windowID)", title: title,
            frame: frame, kind: .window, modal: false, owner_bundle_id: bundle)
        let root = NativeDeviceContextObject(role: kAXWindowRole, subrole: kAXStandardWindowSubrole,
            label: bindingLabel(title), frame: frame)
        let facts = NativeDeviceContextWindowRootFacts(app: app,
            process: NativeMenuProcessIdentity(pid: UInt32(pid), start_seconds: stamp.seconds,
                start_micros: stamp.micros), window: windowFacts, root: root)
        return nativeWindowRootWitnessMatches(expected, actual: facts,
            binding: binding, displayFrame: displayFrame) ? facts : nil
    }
}

struct NativeDeviceContextObject: Codable, Sendable, Equatable {
    let role: String
    let subrole: String?
    let label: String
    let frame: HitRect

    private enum CodingKeys: String, CodingKey { case role, subrole, label, frame }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(role, forKey: .role)
        try values.encode(subrole, forKey: .subrole)
        try values.encode(label, forKey: .label)
        try values.encode(frame, forKey: .frame)
    }
}

struct NativeDeviceContextTarget: Codable, Sendable, Equatable {
    let object: NativeDeviceContextObject
    let group: String?
    let scroll: NativeDeviceContextScrollState?

    private enum CodingKeys: String, CodingKey { case object, group, scroll }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(object, forKey: .object)
        try values.encode(group, forKey: .group)
        try values.encode(scroll, forKey: .scroll)
    }
}

struct NativeDeviceContextScrollAxis: Codable, Sendable, Equatable {
    let value: Double
    let min: Double?
    let max: Double?

    private enum CodingKeys: String, CodingKey { case value, min, max }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(value, forKey: .value)
        try values.encode(min, forKey: .min)
        try values.encode(max, forKey: .max)
    }

    var valid: Bool {
        value.isFinite
            && (min.map { $0.isFinite && value >= $0 } ?? true)
            && (max.map { $0.isFinite && value <= $0 } ?? true)
            && (min.flatMap { lower in max.map { lower < $0 } } ?? true)
    }
}

struct NativeDeviceContextScrollState: Codable, Sendable, Equatable {
    let horizontal: NativeDeviceContextScrollAxis?
    let vertical: NativeDeviceContextScrollAxis?

    private enum CodingKeys: String, CodingKey { case horizontal, vertical }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(horizontal, forKey: .horizontal)
        try values.encode(vertical, forKey: .vertical)
    }

    var valid: Bool {
        (horizontal != nil || vertical != nil)
            && (horizontal?.valid ?? true) && (vertical?.valid ?? true)
    }
}

struct NativeDeviceContextScrollAncestor: Codable, Sendable, Equatable {
    let ancestry_index: UInt8
    let state: NativeDeviceContextScrollState?

    private enum CodingKeys: String, CodingKey { case ancestry_index, state }
    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(ancestry_index, forKey: .ancestry_index)
        try values.encode(state, forKey: .state)
    }
}

struct NativeDeviceContextFingerprint: Codable, Sendable, Equatable {
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

    private enum CodingKeys: String, CodingKey {
        case app, app_pid, window_id, ancestor_roles, group, role, subrole, label, sibling_index, frame
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(app, forKey: .app)
        try values.encode(app_pid, forKey: .app_pid)
        try values.encode(window_id, forKey: .window_id)
        try values.encode(ancestor_roles, forKey: .ancestor_roles)
        try values.encode(group, forKey: .group)
        try values.encode(role, forKey: .role)
        try values.encode(subrole, forKey: .subrole)
        try values.encode(label, forKey: .label)
        try values.encode(sibling_index, forKey: .sibling_index)
        try values.encode(frame, forKey: .frame)
    }

    var valid: Bool {
        nativeID(app) && app.utf8.count <= 256 && app_pid > 0
            && nativeID(window_id) && ancestor_roles.count <= 32
            && ancestor_roles.allSatisfy { !$0.isEmpty && $0.utf8.count <= 64 }
            && (group.map { $0.utf8.count <= 512 && bindingLabel($0) == $0 } ?? true)
            && !role.isEmpty && role.utf8.count <= 64
            && (subrole.map { !$0.isEmpty && $0.utf8.count <= 128 } ?? true)
            && label.utf8.count <= 512 && bindingLabel(label) == label
            && sibling_index < 256 && frame.count == 4
            && frame.allSatisfy { (-100_000...100_000).contains($0) }
            && frame[2] > 0 && frame[3] > 0
    }

    func matches(_ current: NativeBindingFingerprint) -> Bool {
        valid && current.app == app && current.app_pid == app_pid && current.window_id == window_id
            && current.ancestor_roles == ancestor_roles && current.group == group
            && current.role == role && current.subrole == subrole && current.label == label
            && current.sibling_index == sibling_index && current.frame == frame
    }
}

enum NativeDeviceContextMenuContinuity: String, Codable, Sendable, Equatable {
    case directRead = "direct_read"
    case observedLifetime = "observed_lifetime"
}

enum NativeDeviceContextMenuRelation: Codable, Sendable, Equatable {
    case openerAnchor(originWindowID: String, originWindowFrame: HitRect,
                      openerFingerprint: NativeDeviceContextFingerprint,
                      continuity: NativeDeviceContextMenuContinuity)
    case anchoredMenu(originWindowID: String, originWindowFrame: HitRect,
                      openerFingerprint: NativeDeviceContextFingerprint,
                      continuity: NativeDeviceContextMenuContinuity,
                      menuFingerprint: NativeDeviceContextFingerprint,
                      menuFrame: HitRect, drawingWindow: NativeDeviceContextDrawingWindow)

    private enum CodingKeys: String, CodingKey {
        case kind, origin_window_id, origin_window_frame, opener_fingerprint, continuity
        case menu_fingerprint, menu_frame, drawing_window
    }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        let originWindowID = try values.decode(String.self, forKey: .origin_window_id)
        let originWindowFrame = try values.decode(HitRect.self, forKey: .origin_window_frame)
        let openerFingerprint = try values.decode(NativeDeviceContextFingerprint.self, forKey: .opener_fingerprint)
        let continuity = try values.decode(NativeDeviceContextMenuContinuity.self, forKey: .continuity)
        switch try values.decode(String.self, forKey: .kind) {
        case "opener_anchor":
            self = .openerAnchor(originWindowID: originWindowID, originWindowFrame: originWindowFrame,
                openerFingerprint: openerFingerprint, continuity: continuity)
        case "anchored_menu":
            self = .anchoredMenu(originWindowID: originWindowID, originWindowFrame: originWindowFrame,
                openerFingerprint: openerFingerprint, continuity: continuity,
                menuFingerprint: try values.decode(NativeDeviceContextFingerprint.self, forKey: .menu_fingerprint),
                menuFrame: try values.decode(HitRect.self, forKey: .menu_frame),
                drawingWindow: try values.decode(NativeDeviceContextDrawingWindow.self, forKey: .drawing_window))
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values,
                debugDescription: "unknown Device context menu relation")
        }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .openerAnchor(let originWindowID, let originWindowFrame, let openerFingerprint, let continuity):
            try values.encode("opener_anchor", forKey: .kind)
            try values.encode(originWindowID, forKey: .origin_window_id)
            try values.encode(originWindowFrame, forKey: .origin_window_frame)
            try values.encode(openerFingerprint, forKey: .opener_fingerprint)
            try values.encode(continuity, forKey: .continuity)
        case .anchoredMenu(let originWindowID, let originWindowFrame, let openerFingerprint,
                           let continuity, let menuFingerprint, let menuFrame, let drawingWindow):
            try values.encode("anchored_menu", forKey: .kind)
            try values.encode(originWindowID, forKey: .origin_window_id)
            try values.encode(originWindowFrame, forKey: .origin_window_frame)
            try values.encode(openerFingerprint, forKey: .opener_fingerprint)
            try values.encode(continuity, forKey: .continuity)
            try values.encode(menuFingerprint, forKey: .menu_fingerprint)
            try values.encode(menuFrame, forKey: .menu_frame)
            try values.encode(drawingWindow, forKey: .drawing_window)
        }
    }
}

struct NativeDeviceContextDrawingWindow: Codable, Sendable, Equatable {
    let window_id: UInt32
    let owner_pid: UInt32
    let layer: Int32
    let frame: HitRect
}

enum NativeDeviceContextFocusRelationName: String, Codable, Sendable, Equatable {
    case exactControl = "exact_control"
}

enum NativeDeviceContextComposition: String, Codable, Sendable, Equatable {
    case unknown, notApplicable = "not_applicable", marked, unmarked
}

struct NativeDeviceContextFocus: Codable, Sendable, Equatable {
    let origin_window_id: String
    let origin_window_frame: HitRect
    let target_fingerprint: NativeDeviceContextFingerprint
    let target_frame: HitRect
    let relation: NativeDeviceContextFocusRelationName
    let composition: NativeDeviceContextComposition
}

/// Bounded actual AXColumns order for one globally complete sortable header.
/// The AX refs used to check this projection remain worker-local in BindingProof.
struct NativeDeviceContextTableColumn: Codable, Sendable, Equatable {
    let table_ancestry_index: UInt8
    let ordered_ax_indices: [UInt8]
    let target_ax_index: UInt8

    private enum CodingKeys: String, CodingKey {
        case table_ancestry_index, ordered_ax_indices, target_ax_index
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(table_ancestry_index, forKey: .table_ancestry_index)
        try values.encode(ordered_ax_indices, forKey: .ordered_ax_indices)
        try values.encode(target_ax_index, forKey: .target_ax_index)
    }

    var valid: Bool {
        table_ancestry_index < 32
            && ordered_ax_indices.count >= 1 && ordered_ax_indices.count <= 64
            && ordered_ax_indices.allSatisfy { $0 < 64 }
            && Set(ordered_ax_indices).count == ordered_ax_indices.count
            && ordered_ax_indices.filter { $0 == target_ax_index }.count == 1
    }
}

struct NativeDeviceContextPoint: Codable, Sendable, Equatable {
    let x: Double
    let y: Double
}

struct NativeDeviceContextRowColumn: Codable, Sendable, Equatable {
    let ax_index: UInt8
    let header: String
    let frame: HitRect
}

struct NativeDeviceContextRowSelectorColumn: Codable, Sendable, Equatable {
    let ax_index: UInt8
    let header: String
    let expected_value: NativeRowSelectorValue
}

struct NativeDeviceContextVisibleRow: Codable, Sendable, Equatable {
    let ax_index: UInt32
    let frame: HitRect
    let selector_values: [NativeRowSelectorValue]
}

/// Exact public AXEnabled observation used by the dedicated row lane.
/// Attribute absence is distinct from an explicit false value.
enum NativeAXEnabledObservation: String, Codable, Sendable, Equatable {
    case enabled
    case disabled
    case attributeUnsupported = "attribute_unsupported"
    case unavailable

    var permitsRowSelection: Bool {
        self == .enabled || self == .attributeUnsupported
    }
}

/// A bounded value-only description for the dedicated AXSelected=true lane.
/// It carries no Source registry refs and is compared with Device-local AX refs.
struct NativeDeviceContextTableViewportV1: Codable, Sendable, Equatable {
    let app: NativeDeviceContextApp
    let window: NativeDeviceContextWindow
    let table: NativeDeviceContextObject
    let target: NativeDeviceContextTarget
    let scroll_path: [NativeDeviceContextObject]
    let viewport_frame: HitRect
    let ordered_columns: [NativeDeviceContextRowColumn]
    let selector_columns: [NativeDeviceContextRowSelectorColumn]
    let visible_rows: [NativeDeviceContextVisibleRow]
    let target_row_ax_index: UInt32
    let table_ax_enabled: NativeAXEnabledObservation
    let target_ax_enabled: NativeAXEnabledObservation
    let target_selected: Bool
    let target_ax_selected_settable: Bool
    let hit_point: NativeDeviceContextPoint

    private enum CodingKeys: String, CodingKey {
        case app, window, table, target, scroll_path, viewport_frame, ordered_columns
        case selector_columns, visible_rows, target_row_ax_index, table_ax_enabled
        case target_ax_enabled, target_selected
        case target_ax_selected_settable, hit_point
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        try values.encode(app, forKey: .app)
        try values.encode(window, forKey: .window)
        try values.encode(table, forKey: .table)
        try values.encode(target, forKey: .target)
        try values.encode(scroll_path, forKey: .scroll_path)
        try values.encode(viewport_frame, forKey: .viewport_frame)
        try values.encode(ordered_columns, forKey: .ordered_columns)
        try values.encode(selector_columns, forKey: .selector_columns)
        try values.encode(visible_rows, forKey: .visible_rows)
        try values.encode(target_row_ax_index, forKey: .target_row_ax_index)
        try values.encode(table_ax_enabled, forKey: .table_ax_enabled)
        try values.encode(target_ax_enabled, forKey: .target_ax_enabled)
        try values.encode(target_selected, forKey: .target_selected)
        try values.encode(target_ax_selected_settable, forKey: .target_ax_selected_settable)
        try values.encode(hit_point, forKey: .hit_point)
    }

    var valid: Bool {
        let bounded: (String, Int) -> Bool = { text, maximum in
            text.utf8.count <= maximum
                && !text.unicodeScalars.contains { $0.properties.generalCategory == .control }
        }
        func contains(_ outer: HitRect, _ inner: HitRect) -> Bool {
            outer.valid && inner.valid && inner.x >= outer.x && inner.y >= outer.y
                && inner.x + inner.width <= outer.x + outer.width
                && inner.y + inner.height <= outer.y + outer.height
        }
        func headerKey(_ text: String) -> String {
            text.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        }

        guard nativeID(app.bundle_id), app.pid > 0, app.pid <= UInt32(Int32.max),
              nativeID(window.window_id), window.owner_bundle_id == app.bundle_id,
              bounded(window.title ?? "", 512), window.frame.valid,
              (window.kind == .window && !window.modal) || (window.kind == .sheet && window.modal),
              table.role == kAXTableRole, bounded(table.label, 512), table.frame.valid,
              target.object.role == kAXRowRole, target.object.subrole == "AXTableRow",
              bounded(target.object.label, 512), bindingLabel(target.object.label) == target.object.label,
              target.object.frame.valid, target.scroll == nil,
              target.group.map({ $0.utf8.count <= 512 && bindingLabel($0) == $0 }) ?? true,
              viewport_frame.valid, contains(window.frame, viewport_frame),
              table.frame.rect.intersects(viewport_frame.rect),
              !scroll_path.isEmpty, scroll_path.count <= 8,
              ordered_columns.count > 0, ordered_columns.count <= NativeRowBindingValidation.maximumColumns,
              selector_columns.count > 0,
              selector_columns.count <= NativeRowBindingValidation.maximumSelectorConditions,
              visible_rows.count > 0, visible_rows.count <= NativeRowBindingValidation.maximumVisibleRows,
              table_ax_enabled.permitsRowSelection, target_ax_enabled.permitsRowSelection,
              hit_point.x.isFinite, hit_point.y.isFinite,
              abs(hit_point.x) <= 100_000, abs(hit_point.y) <= 100_000,
              !target_selected, target_ax_selected_settable else { return false }

        for (index, scroll) in scroll_path.enumerated() {
            guard scroll.role == kAXScrollAreaRole, scroll.frame.valid,
                  !scroll.label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  bounded(scroll.label, 512), contains(scroll.frame, viewport_frame),
                  index == 0 || contains(scroll_path[index - 1].frame, scroll.frame) else { return false }
        }

        var columnIndices = Set<UInt8>()
        var columnHeaders = Set<String>()
        for column in ordered_columns {
            guard column.ax_index < UInt8(NativeRowBindingValidation.maximumColumns),
                  !column.header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  bounded(column.header, 256), columnIndices.insert(column.ax_index).inserted,
                  columnHeaders.insert(headerKey(column.header)).inserted,
                  column.frame.valid, table.frame.rect.intersects(column.frame.rect) else {
                return false
            }
        }

        var selectorIndices = Set<UInt8>()
        var selectorHeaders = Set<String>()
        for selector in selector_columns {
            guard selector.ax_index < UInt8(NativeRowBindingValidation.maximumColumns),
                  !selector.header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  bounded(selector.header, 256), selectorIndices.insert(selector.ax_index).inserted,
                  selectorHeaders.insert(headerKey(selector.header)).inserted,
                  ordered_columns.contains(where: {
                      $0.ax_index == selector.ax_index && $0.header == selector.header
                  }), selector.expected_value.valid else { return false }
        }

        var rowIndices = Set<UInt32>()
        var selectorMatches: [UInt32] = []
        var targetFrame: HitRect?
        for row in visible_rows {
            guard rowIndices.insert(row.ax_index).inserted,
                  row.frame.valid, contains(table.frame, row.frame),
                  row.frame.rect.intersects(viewport_frame.rect),
                  row.selector_values.count == selector_columns.count,
                  row.selector_values.allSatisfy(\.valid) else { return false }
            if zip(row.selector_values, selector_columns).allSatisfy({ pair in
                pair.0 == pair.1.expected_value
            }) {
                selectorMatches.append(row.ax_index)
            }
            if row.ax_index == target_row_ax_index { targetFrame = row.frame }
        }
        guard selectorMatches == [target_row_ax_index], let targetFrame,
              targetFrame == target.object.frame,
              viewport_frame.contains(x: hit_point.x, y: hit_point.y),
              targetFrame.contains(x: hit_point.x, y: hit_point.y) else { return false }
        return true
    }
}

enum NativeDeviceContextUnsupportedReason: String, Codable, Sendable, Equatable {
    case tableViewport = "table_viewport"
}

struct NativeDeviceContextWholeStructure: Codable, Sendable, Equatable {
    let app: NativeDeviceContextApp
    let active_layer: String?
    let window: NativeDeviceContextWindow
    let target: NativeDeviceContextTarget
    let ancestry: [NativeDeviceContextObject]
    let sibling_parent_ancestry_index: UInt8?
    let siblings: [NativeDeviceContextObject]
    let target_sibling_index: UInt16
    let nearest_labelled_group_ancestry_index: UInt8?
    let scroll_ancestors: [NativeDeviceContextScrollAncestor]
    let menu: NativeDeviceContextMenuRelation?
    let focus: NativeDeviceContextFocus?
    let table_column: NativeDeviceContextTableColumn?

    init(app: NativeDeviceContextApp, active_layer: String?, window: NativeDeviceContextWindow,
         target: NativeDeviceContextTarget, ancestry: [NativeDeviceContextObject],
         sibling_parent_ancestry_index: UInt8?, siblings: [NativeDeviceContextObject],
         target_sibling_index: UInt16, nearest_labelled_group_ancestry_index: UInt8?,
         scroll_ancestors: [NativeDeviceContextScrollAncestor], menu: NativeDeviceContextMenuRelation?,
         focus: NativeDeviceContextFocus?, table_column: NativeDeviceContextTableColumn? = nil) {
        self.app = app
        self.active_layer = active_layer
        self.window = window
        self.target = target
        self.ancestry = ancestry
        self.sibling_parent_ancestry_index = sibling_parent_ancestry_index
        self.siblings = siblings
        self.target_sibling_index = target_sibling_index
        self.nearest_labelled_group_ancestry_index = nearest_labelled_group_ancestry_index
        self.scroll_ancestors = scroll_ancestors
        self.menu = menu
        self.focus = focus
        self.table_column = table_column
    }
}

/// A separately tagged, purpose-bound background AX witness. Its facts overlap
/// with WholeStructure internally, but its wire identity and validation remain
/// independent and it grants only a single AXPress in BackgroundAX mode.
struct NativeDeviceContextBackgroundAXV1: Codable, Sendable, Equatable {
    let app: NativeDeviceContextApp
    let window: NativeDeviceContextWindow
    let root: NativeDeviceContextObject
    let target: NativeDeviceContextTarget
    let ancestry: [NativeDeviceContextObject]
    let sibling_parent_ancestry_index: UInt8
    let siblings: [NativeDeviceContextObject]
    let target_sibling_index: UInt16
    let nearest_labelled_group_ancestry_index: UInt8?
    let source: NativeDeviceSourceBackgroundWindowContext
    let target_context_hash: String
    let target_enabled: Bool
    let target_action_names: [String]

    private enum CodingKeys: String, CodingKey {
        case app, window, root, target, ancestry, sibling_parent_ancestry_index, siblings
        case target_sibling_index, nearest_labelled_group_ancestry_index, source
        case target_context_hash, target_enabled, target_action_names
    }

    var wholeStructureFacts: NativeDeviceContextWholeStructure {
        NativeDeviceContextWholeStructure(app: app, active_layer: nil, window: window, target: target,
            ancestry: ancestry, sibling_parent_ancestry_index: sibling_parent_ancestry_index,
            siblings: siblings, target_sibling_index: target_sibling_index,
            nearest_labelled_group_ancestry_index: nearest_labelled_group_ancestry_index,
            scroll_ancestors: [], menu: nil, focus: nil, table_column: nil)
    }

    var valid: Bool {
        guard app.pid > 0, app.pid <= UInt32(Int32.max), nativeID(app.bundle_id),
              nativeID(window.window_id), window.window_id.hasPrefix("window-"),
              UInt32(window.window_id.dropFirst(7)).map({ $0 > 0 }) == true,
              window.kind == .window, !window.modal, window.owner_bundle_id == app.bundle_id,
              window.frame.valid,
              window.title.map({ $0.utf8.count <= 512 && bindingLabel($0) == $0
                  && !$0.unicodeScalars.contains { $0.properties.generalCategory == .control } }) ?? true,
              root.role == kAXWindowRole, root.subrole == kAXStandardWindowSubrole,
              root.frame == window.frame, root.label == bindingLabel(window.title ?? ""),
              source.valid(bundleID: app.bundle_id, pid: app.pid,
                           windowID: window.window_id, frame: window.frame),
              source.hidden.isKnownFalse, source.minimized.isKnownFalse,
              target_context_hash == source.target_context_hash,
              target.object.role != kAXWindowRole, target.scroll == nil,
              ancestry.count >= 1, ancestry.count <= 32, ancestry.first == root,
              !ancestry.contains(where: { $0.role == kAXScrollAreaRole }),
              !(ancestry + siblings).contains(where: {
                  [kAXSheetRole, "AXDialog", "AXPopover", kAXMenuRole].contains($0.role)
              }),
              Int(sibling_parent_ancestry_index) == ancestry.count - 1,
              !siblings.isEmpty, siblings.count <= 256,
              Int(target_sibling_index) < siblings.count,
              siblings[Int(target_sibling_index)] == target.object,
              target_enabled, validActionSet(target_action_names),
              target.object.frame.valid else { return false }
        let objects = ancestry + siblings + [target.object]
        guard objects.allSatisfy({
            !$0.role.isEmpty && $0.role.utf8.count <= 64
                && ($0.subrole.map { !$0.isEmpty && $0.utf8.count <= 128 } ?? true)
                && $0.label.utf8.count <= 512 && bindingLabel($0.label) == $0.label
                && !$0.label.unicodeScalars.contains { $0.properties.generalCategory == .control }
                && $0.frame.valid
        }) else { return false }
        let nearestGroup = ancestry.indices.reversed().first { index in
            isBindingGroupRole(ancestry[index].role) && !ancestry[index].label.isEmpty
        }.map { UInt8($0) }
        guard nearest_labelled_group_ancestry_index == nearestGroup else { return false }
        if let nearestGroup {
            guard target.group == ancestry[Int(nearestGroup)].label else { return false }
        } else if target.group != nil {
            return false
        }
        guard target.group.map({ $0.utf8.count <= 512 && bindingLabel($0) == $0 }) ?? true,
              let encoded = try? JSONEncoder().encode(wholeStructureFacts) else { return false }
        return encoded.count <= 4096
    }

    private func validActionSet(_ names: [String]) -> Bool {
        guard !names.isEmpty, names.count <= 32 else { return false }
        var seen = Set<String>()
        return names == names.sorted() && names.allSatisfy { name in
            !name.isEmpty && name.utf8.count <= 64
                && !name.unicodeScalars.contains { $0.properties.generalCategory == .control }
                && seen.insert(name).inserted
        } && seen.contains(kAXPressAction as String)
    }
}

enum NativeDeviceContextWitness: Codable, Sendable, Equatable {
    case wholeStructureV1(NativeDeviceContextWholeStructure)
    case tableViewportV1(NativeDeviceContextTableViewportV1)
    case windowRootV1(NativeDeviceContextWindowRootV1)
    case backgroundAxV1(NativeDeviceContextBackgroundAXV1)
    case unsupported(NativeDeviceContextUnsupportedReason)

    private enum CodingKeys: String, CodingKey {
        case scope, reason, app, active_layer, window, target, ancestry
        case sibling_parent_ancestry_index, siblings, target_sibling_index
        case nearest_labelled_group_ancestry_index, scroll_ancestors, menu, focus, table_column
        case table, scroll_path, viewport_frame, ordered_columns, selector_columns, visible_rows
        case target_row_ax_index, table_ax_enabled, target_ax_enabled, target_selected
        case target_ax_selected_settable, hit_point
        case root, source
        case target_context_hash, target_enabled, target_action_names
    }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .scope) {
        case "whole_structure_v1":
            self = .wholeStructureV1(NativeDeviceContextWholeStructure(
                app: try values.decode(NativeDeviceContextApp.self, forKey: .app),
                active_layer: try values.decodeIfPresent(String.self, forKey: .active_layer),
                window: try values.decode(NativeDeviceContextWindow.self, forKey: .window),
                target: try values.decode(NativeDeviceContextTarget.self, forKey: .target),
                ancestry: try values.decode([NativeDeviceContextObject].self, forKey: .ancestry),
                sibling_parent_ancestry_index: try values.decodeIfPresent(UInt8.self, forKey: .sibling_parent_ancestry_index),
                siblings: try values.decode([NativeDeviceContextObject].self, forKey: .siblings),
                target_sibling_index: try values.decode(UInt16.self, forKey: .target_sibling_index),
                nearest_labelled_group_ancestry_index: try values.decodeIfPresent(UInt8.self, forKey: .nearest_labelled_group_ancestry_index),
                scroll_ancestors: try values.decode([NativeDeviceContextScrollAncestor].self, forKey: .scroll_ancestors),
                menu: try values.decodeIfPresent(NativeDeviceContextMenuRelation.self, forKey: .menu),
                focus: try values.decodeIfPresent(NativeDeviceContextFocus.self, forKey: .focus),
                table_column: try values.decodeIfPresent(NativeDeviceContextTableColumn.self, forKey: .table_column)))
        case "table_viewport_v1":
            self = .tableViewportV1(NativeDeviceContextTableViewportV1(
                app: try values.decode(NativeDeviceContextApp.self, forKey: .app),
                window: try values.decode(NativeDeviceContextWindow.self, forKey: .window),
                table: try values.decode(NativeDeviceContextObject.self, forKey: .table),
                target: try values.decode(NativeDeviceContextTarget.self, forKey: .target),
                scroll_path: try values.decode([NativeDeviceContextObject].self, forKey: .scroll_path),
                viewport_frame: try values.decode(HitRect.self, forKey: .viewport_frame),
                ordered_columns: try values.decode([NativeDeviceContextRowColumn].self, forKey: .ordered_columns),
                selector_columns: try values.decode([NativeDeviceContextRowSelectorColumn].self, forKey: .selector_columns),
                visible_rows: try values.decode([NativeDeviceContextVisibleRow].self, forKey: .visible_rows),
                target_row_ax_index: try values.decode(UInt32.self, forKey: .target_row_ax_index),
                table_ax_enabled: try values.decode(NativeAXEnabledObservation.self, forKey: .table_ax_enabled),
                target_ax_enabled: try values.decode(NativeAXEnabledObservation.self, forKey: .target_ax_enabled),
                target_selected: try values.decode(Bool.self, forKey: .target_selected),
                target_ax_selected_settable: try values.decode(Bool.self, forKey: .target_ax_selected_settable),
                hit_point: try values.decode(NativeDeviceContextPoint.self, forKey: .hit_point)))
        case "window_root_v1":
            self = .windowRootV1(NativeDeviceContextWindowRootV1(
                app: try values.decode(NativeDeviceContextApp.self, forKey: .app),
                window: try values.decode(NativeDeviceContextWindow.self, forKey: .window),
                root: try values.decode(NativeDeviceContextObject.self, forKey: .root),
                source: try values.decode(NativeDeviceSourceWindowContext.self, forKey: .source)))
        case "background_ax_v1":
            self = .backgroundAxV1(NativeDeviceContextBackgroundAXV1(
                app: try values.decode(NativeDeviceContextApp.self, forKey: .app),
                window: try values.decode(NativeDeviceContextWindow.self, forKey: .window),
                root: try values.decode(NativeDeviceContextObject.self, forKey: .root),
                target: try values.decode(NativeDeviceContextTarget.self, forKey: .target),
                ancestry: try values.decode([NativeDeviceContextObject].self, forKey: .ancestry),
                sibling_parent_ancestry_index: try values.decode(UInt8.self, forKey: .sibling_parent_ancestry_index),
                siblings: try values.decode([NativeDeviceContextObject].self, forKey: .siblings),
                target_sibling_index: try values.decode(UInt16.self, forKey: .target_sibling_index),
                nearest_labelled_group_ancestry_index: try values.decodeIfPresent(UInt8.self, forKey: .nearest_labelled_group_ancestry_index),
                source: try values.decode(NativeDeviceSourceBackgroundWindowContext.self, forKey: .source),
                target_context_hash: try values.decode(String.self, forKey: .target_context_hash),
                target_enabled: try values.decode(Bool.self, forKey: .target_enabled),
                target_action_names: try values.decode([String].self, forKey: .target_action_names)))
        case "unsupported":
            self = .unsupported(try values.decode(NativeDeviceContextUnsupportedReason.self, forKey: .reason))
        default:
            throw DecodingError.dataCorruptedError(forKey: .scope, in: values,
                debugDescription: "unknown Device context witness scope")
        }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .wholeStructureV1(let context):
            try values.encode("whole_structure_v1", forKey: .scope)
            try values.encode(context.app, forKey: .app)
            try values.encode(context.active_layer, forKey: .active_layer)
            try values.encode(context.window, forKey: .window)
            try values.encode(context.target, forKey: .target)
            try values.encode(context.ancestry, forKey: .ancestry)
            try values.encode(context.sibling_parent_ancestry_index, forKey: .sibling_parent_ancestry_index)
            try values.encode(context.siblings, forKey: .siblings)
            try values.encode(context.target_sibling_index, forKey: .target_sibling_index)
            try values.encode(context.nearest_labelled_group_ancestry_index, forKey: .nearest_labelled_group_ancestry_index)
            try values.encode(context.scroll_ancestors, forKey: .scroll_ancestors)
            try values.encode(context.menu, forKey: .menu)
            try values.encode(context.focus, forKey: .focus)
            try values.encode(context.table_column, forKey: .table_column)
        case .tableViewportV1(let context):
            try values.encode("table_viewport_v1", forKey: .scope)
            try values.encode(context.app, forKey: .app)
            try values.encode(context.window, forKey: .window)
            try values.encode(context.table, forKey: .table)
            try values.encode(context.target, forKey: .target)
            try values.encode(context.scroll_path, forKey: .scroll_path)
            try values.encode(context.viewport_frame, forKey: .viewport_frame)
            try values.encode(context.ordered_columns, forKey: .ordered_columns)
            try values.encode(context.selector_columns, forKey: .selector_columns)
            try values.encode(context.visible_rows, forKey: .visible_rows)
            try values.encode(context.target_row_ax_index, forKey: .target_row_ax_index)
            try values.encode(context.table_ax_enabled, forKey: .table_ax_enabled)
            try values.encode(context.target_ax_enabled, forKey: .target_ax_enabled)
            try values.encode(context.target_selected, forKey: .target_selected)
            try values.encode(context.target_ax_selected_settable, forKey: .target_ax_selected_settable)
            try values.encode(context.hit_point, forKey: .hit_point)
        case .windowRootV1(let context):
            try values.encode("window_root_v1", forKey: .scope)
            try values.encode(context.app, forKey: .app)
            try values.encode(context.window, forKey: .window)
            try values.encode(context.root, forKey: .root)
            try values.encode(context.source, forKey: .source)
        case .backgroundAxV1(let context):
            try values.encode("background_ax_v1", forKey: .scope)
            try values.encode(context.app, forKey: .app)
            try values.encode(context.window, forKey: .window)
            try values.encode(context.root, forKey: .root)
            try values.encode(context.target, forKey: .target)
            try values.encode(context.ancestry, forKey: .ancestry)
            try values.encode(context.sibling_parent_ancestry_index, forKey: .sibling_parent_ancestry_index)
            try values.encode(context.siblings, forKey: .siblings)
            try values.encode(context.target_sibling_index, forKey: .target_sibling_index)
            try values.encode(context.nearest_labelled_group_ancestry_index, forKey: .nearest_labelled_group_ancestry_index)
            try values.encode(context.source, forKey: .source)
            try values.encode(context.target_context_hash, forKey: .target_context_hash)
            try values.encode(context.target_enabled, forKey: .target_enabled)
            try values.encode(context.target_action_names, forKey: .target_action_names)
        case .unsupported(let reason):
            try values.encode("unsupported", forKey: .scope)
            try values.encode(reason, forKey: .reason)
        }
    }

    static let wholeStructureKeys: Set<String> = ["scope", "app", "active_layer", "window", "target",
        "ancestry", "sibling_parent_ancestry_index", "siblings", "target_sibling_index",
        "nearest_labelled_group_ancestry_index", "scroll_ancestors", "menu", "focus", "table_column"]

    static let tableViewportV1Keys: Set<String> = ["scope", "app", "window", "table", "target",
        "scroll_path", "viewport_frame", "ordered_columns", "selector_columns", "visible_rows",
        "target_row_ax_index", "table_ax_enabled", "target_ax_enabled", "target_selected",
        "target_ax_selected_settable", "hit_point"]

    static let windowRootV1Keys: Set<String> = ["scope", "app", "window", "root", "source"]
    static let backgroundAxV1Keys: Set<String> = ["scope", "app", "window", "root", "target", "ancestry",
        "sibling_parent_ancestry_index", "siblings", "target_sibling_index",
        "nearest_labelled_group_ancestry_index", "source", "target_context_hash",
        "target_enabled", "target_action_names"]

    var supportsNativeInput: Bool {
        if case .wholeStructureV1 = self { return true }
        return false
    }

    func supportsNativeInput(action: String) -> Bool {
        switch self {
        case .wholeStructureV1:
            return true
        case .tableViewportV1(let context):
            return action == "ax_set_selected_true" && context.valid
        case .windowRootV1(let context):
            return action == "activate_window" && context.valid
        case .backgroundAxV1(let context):
            return action == "ax_press" && context.valid
        case .unsupported:
            return false
        }
    }

    var valid: Bool {
        guard let encoded = try? JSONEncoder().encode(self), encoded.count <= 4096 else { return false }
        if case .tableViewportV1(let context) = self { return context.valid }
        if case .windowRootV1(let context) = self { return context.valid }
        if case .backgroundAxV1(let context) = self { return context.valid }
        guard case .wholeStructureV1(let context) = self else {
            if case .unsupported(.tableViewport) = self { return true }
            return false
        }
        let bounded: (String, Int) -> Bool = { text, maximum in
            !text.isEmpty && text.utf8.count <= maximum
                && !text.unicodeScalars.contains { $0.properties.generalCategory == .control }
        }
        guard bounded(context.app.bundle_id, 256), context.app.pid > 0,
              context.active_layer.map({ bounded($0, 256) }) ?? true,
              bounded(context.window.window_id, 256),
              context.window.owner_bundle_id == context.app.bundle_id,
              context.window.title.map({ $0.utf8.count <= 512
                  && !$0.unicodeScalars.contains { $0.properties.generalCategory == .control }
                  && bindingLabel($0) == $0 }) ?? true,
              context.window.frame.valid,
              context.ancestry.count <= 32,
              !context.siblings.isEmpty, context.siblings.count <= 256,
              Int(context.target_sibling_index) < context.siblings.count,
              context.siblings[Int(context.target_sibling_index)] == context.target.object else { return false }
        for object in context.ancestry + context.siblings + [context.target.object] {
            guard bounded(object.role, 64),
                  object.subrole.map({ bounded($0, 128) }) ?? true,
                  object.label.utf8.count <= 512, bindingLabel(object.label) == object.label,
                  object.frame.valid else { return false }
        }
        if context.ancestry.isEmpty {
            guard context.sibling_parent_ancestry_index == nil else { return false }
        } else {
            guard context.sibling_parent_ancestry_index == UInt8(context.ancestry.count - 1) else { return false }
        }
        if let index = context.nearest_labelled_group_ancestry_index {
            guard Int(index) < context.ancestry.count else { return false }
            let group = context.ancestry[Int(index)]
            guard isBindingGroupRole(group.role), !group.label.isEmpty,
                  context.target.group.map({ $0 == group.label }) ?? true else { return false }
        } else if context.target.group.map({ !$0.isEmpty }) ?? false {
            return false
        }
        guard context.target.group.map({ $0.utf8.count <= 512 && bindingLabel($0) == $0 }) ?? true,
              context.target.scroll.map(\.valid) ?? true,
              context.scroll_ancestors.count <= 32 else { return false }
        var scrollIndexes = Set<UInt8>()
        for scroll in context.scroll_ancestors {
            guard Int(scroll.ancestry_index) < context.ancestry.count,
                  context.ancestry[Int(scroll.ancestry_index)].role == kAXScrollAreaRole,
                  scrollIndexes.insert(scroll.ancestry_index).inserted,
                  scroll.state.map(\.valid) ?? true else { return false }
        }
        let root = context.ancestry.first ?? context.target.object
        guard root.frame == context.window.frame else { return false }
        let rootMatchesWindow = (root.role == kAXWindowRole && context.window.kind == .window && !context.window.modal)
            || (root.role == kAXSheetRole && context.window.kind == .sheet && context.window.modal)
            || (root.role == kAXMenuRole && context.window.kind == .menu)
        guard rootMatchesWindow,
              context.menu.map({ validMenuContext($0, app: context.app) }) ?? true,
              context.focus.map({ validFocusContext($0, app: context.app) }) ?? true else { return false }
        let allBindingRoles = context.ancestry.map(\.role) + [context.target.object.role]
        let tableRoles = [kAXTableRole, "AXOutline"]
        let tableRelated = allBindingRoles.contains { tableRoles.contains($0) }
            || (context.target.object.role == kAXButtonRole && context.target.object.subrole == "AXSortButton")
        if let tableColumn = context.table_column {
            let tableIndexes = context.ancestry.indices.filter { context.ancestry[$0].role == kAXTableRole }
            guard tableColumn.valid, tableRelated,
                  !allBindingRoles.contains("AXOutline"),
                  context.target.object.role == kAXButtonRole,
                  context.target.object.subrole == "AXSortButton",
                  context.menu == nil, context.focus == nil,
                  tableIndexes.count == 1,
                  Int(tableColumn.table_ancestry_index) == tableIndexes[0] else { return false }
        } else if tableRelated {
            return false
        }
        return true
    }

    var tableColumn: NativeDeviceContextTableColumn? {
        guard case .wholeStructureV1(let context) = self else { return nil }
        return context.table_column
    }

    var tableViewport: NativeDeviceContextTableViewportV1? {
        guard case .tableViewportV1(let context) = self else { return nil }
        return context
    }

    var windowRootV1: NativeDeviceContextWindowRootV1? {
        guard case .windowRootV1(let context) = self else { return nil }
        return context
    }

    var backgroundAXV1: NativeDeviceContextBackgroundAXV1? {
        guard case .backgroundAxV1(let context) = self else { return nil }
        return context
    }

    func supportsAction(_ action: String) -> Bool {
        switch self {
        case .wholeStructureV1:
            guard action != "ax_set_selected_true" else { return false }
            guard let tableColumn else { return true }
            return tableColumn.valid && action == "ax_press"
        case .tableViewportV1(let context):
            return context.valid && action == "ax_set_selected_true"
        case .windowRootV1(let context):
            return context.valid && action == "activate_window"
        case .backgroundAxV1(let context):
            return context.valid && action == "ax_press"
        case .unsupported:
            return false
        }
    }

    func matchesBinding(_ binding: NativeBindingFingerprint, displayFrame: HitRect) -> Bool {
        guard valid else { return false }
        switch self {
        case .wholeStructureV1(let context):
            return context.app.bundle_id == binding.app && context.app.pid == binding.app_pid
                && context.window.window_id == binding.window_id
                && context.target.object.role == binding.role
                && context.target.object.subrole == binding.subrole
                && context.target.object.label == binding.label
                && context.target.group == binding.group
                && context.target.object.frame == displayFrame
                && displayFrame.rounded == binding.frame
                && context.ancestry.map(\.role) == binding.ancestor_roles
                && UInt32(context.target_sibling_index) == binding.sibling_index
        case .tableViewportV1(let context):
            let sourceScrollRoles = binding.ancestor_roles.filter { $0 == kAXScrollAreaRole }
            return context.app.bundle_id == binding.app && context.app.pid == binding.app_pid
                && context.window.window_id == binding.window_id
                && context.target.object.role == binding.role
                && context.target.object.subrole == binding.subrole
                && context.target.object.label == binding.label
                && context.target.group == binding.group
                && context.target.object.frame == displayFrame
                && displayFrame.rounded == binding.frame
                && binding.role == kAXRowRole && binding.subrole == "AXTableRow"
                && binding.ancestor_roles.last == kAXTableRole
                && binding.ancestor_roles.filter { $0 == kAXTableRole }.count == 1
                && context.scroll_path.map(\.role) == sourceScrollRoles
        case .windowRootV1(let context):
            return nativeWindowRootWitnessMatches(context,
                actual: NativeDeviceContextWindowRootFacts(app: context.app,
                    process: context.source.process, window: context.window, root: context.root),
                binding: binding, displayFrame: displayFrame)
        case .backgroundAxV1(let context):
            return context.valid && context.app.bundle_id == binding.app
                && context.app.pid == binding.app_pid && context.window.window_id == binding.window_id
                && context.target.object.role == binding.role
                && context.target.object.subrole == binding.subrole
                && context.target.object.label == binding.label
                && context.target.group == binding.group
                && context.target.object.frame == displayFrame
                && displayFrame.rounded == binding.frame
                && context.ancestry.map(\.role) == binding.ancestor_roles
                && UInt32(context.target_sibling_index) == binding.sibling_index
        case .unsupported:
            return false
        }
    }

    func matchesScopes(menuScope: NativeMenuBindingScope?,
                       focusScope: NativeFocusedControlScope?) -> Bool {
        if case .windowRootV1(let context) = self {
            return context.valid && menuScope == nil && focusScope == nil
        }
        if case .tableViewportV1(let context) = self {
            return context.valid && menuScope == nil && focusScope == nil
        }
        if case .backgroundAxV1(let context) = self {
            return context.valid && menuScope == nil && focusScope == nil
        }
        guard case .wholeStructureV1(let context) = self else { return false }
        if let menuScope {
            guard let menu = context.menu else { return false }
            switch (menuScope, menu) {
            case let (.opener(anchor, _), .openerAnchor(originID, originFrame, opener, continuity)),
                 let (.opener(anchor, _), .anchoredMenu(originID, originFrame, opener, continuity, _, _, _)):
                guard originID == anchor.origin_window_id && originFrame == anchor.origin_window_frame
                    && opener.matches(anchor.opener_fingerprint)
                    && continuity.rawValue == anchor.continuity else { return false }
            case let (.item(nativeContext, _), .anchoredMenu(originID, originFrame, opener, continuity,
                                                              menuFingerprint, menuFrame, drawingWindow)):
                guard originID == nativeContext.opener.origin_window_id
                    && originFrame == nativeContext.opener.origin_window_frame
                    && opener.matches(nativeContext.opener.opener_fingerprint)
                    && continuity.rawValue == nativeContext.opener.continuity
                    && menuFingerprint.matches(nativeContext.menu_fingerprint)
                    && menuFrame == nativeContext.menu_frame
                    && drawingWindow.window_id == nativeContext.drawing_window.window_id
                    && drawingWindow.owner_pid == nativeContext.drawing_window.owner_pid
                    && drawingWindow.layer == nativeContext.drawing_window.layer
                    && drawingWindow.frame == nativeContext.drawing_window.frame else { return false }
            default:
                return false
            }
        }
        if let focusScope {
            guard let focus = context.focus, focus.relation.rawValue == focusScope.context.relation,
                  focus.origin_window_id == focusScope.context.origin_window_id,
                  focus.origin_window_frame == focusScope.context.origin_window_frame,
                  focus.target_fingerprint.matches(focusScope.context.target_fingerprint),
                  focus.target_frame == focusScope.context.target_frame,
                  focus.composition == .notApplicable else { return false }
        }
        return true
    }
}

private func isBindingGroupRole(_ role: String) -> Bool {
    role == kAXGroupRole || role.hasSuffix("Group")
}

extension NativeBindingContextSnapshot {
    func matches(_ witness: NativeDeviceContextWitness,
                 binding: NativeBindingFingerprint,
                 displayFrame: HitRect,
                 activeLayer: String?,
        verifyActiveLayer: Bool) -> Bool {
        guard connectedPath(),
              witness.matchesBinding(binding, displayFrame: displayFrame),
              case .wholeStructureV1(let expected) = witness,
              let targetNode = path.first, let rootNode = path.last,
              expected.app.bundle_id == binding.app, expected.app.pid == binding.app_pid,
              expected.window.window_id == binding.window_id,
              expected.window.title == windowTitle,
              expected.window.frame == rootNode.frame,
              expected.window.owner_bundle_id == binding.app,
              (!verifyActiveLayer || expected.active_layer == activeLayer) else { return false }

        let expectedWindowLayer = (rootNode.role == kAXWindowRole && expected.window.kind == .window && !expected.window.modal)
            || (rootNode.role == kAXSheetRole && sheetParent != nil && expected.window.kind == .sheet && expected.window.modal)
            || (rootNode.role == kAXMenuRole && expected.window.kind == .menu)
        guard expectedWindowLayer else { return false }

        let actualAncestry = Array(path.dropFirst().reversed())
        let actualObjects = actualAncestry.map(deviceContextObject)
        let actualSiblings = targetSiblings.map { sibling in
            NativeDeviceContextObject(role: sibling.role, subrole: sibling.subrole,
                label: sibling.label, frame: sibling.frame)
        }
        let actualTarget = deviceContextObject(targetNode)
        guard let targetIndex = targetNode.siblingIndex,
              targetIndex <= UInt32(UInt16.max) else { return false }
        let expectedParentIndex = actualAncestry.isEmpty ? nil : UInt8(actualAncestry.count - 1)
        let nearestGroupIndex = actualAncestry.indices.reversed().first { index in
            let node = actualAncestry[index]
            return isBindingGroupRole(node.role) && !node.label.isEmpty
        }.map { UInt8($0) }
        let actualScrollAncestors = actualAncestry.enumerated().compactMap { index, node -> NativeDeviceContextScrollAncestor? in
            guard node.role == kAXScrollAreaRole else { return nil }
            return NativeDeviceContextScrollAncestor(ancestry_index: UInt8(index), state: deviceContextScroll(node.scroll))
        }

        guard expected.target.object == actualTarget,
              expected.target.group == targetNode.group,
              expected.target.scroll == deviceContextScroll(targetNode.scroll),
              expected.ancestry == actualObjects,
              expected.sibling_parent_ancestry_index == expectedParentIndex,
              expected.siblings == actualSiblings,
              expected.target_sibling_index == UInt16(targetIndex),
              expected.nearest_labelled_group_ancestry_index == nearestGroupIndex,
              expected.scroll_ancestors == actualScrollAncestors,
              Int(targetIndex) < targetSiblings.count,
              CFEqual(targetNode.element, targetSiblings[Int(targetIndex)].element) else { return false }
        if let tableColumn = expected.table_column {
            let tableNodes = actualAncestry.enumerated().filter { $0.element.role == kAXTableRole }
            guard let graph = tableColumnGraph,
                  tableNodes.count == 1,
                  tableNodes[0].offset == Int(tableColumn.table_ancestry_index),
                  CFEqual(graph.table, tableNodes[0].element.element),
                  graph.matches(tableColumn, target: targetNode.element) else { return false }
        } else if tableColumnGraph != nil {
            return false
        }
        return true
    }
}

private func deviceContextObject(_ node: NativeBindingPathNode) -> NativeDeviceContextObject {
    NativeDeviceContextObject(role: node.role, subrole: node.subrole, label: node.label, frame: node.frame)
}

private func deviceContextScroll(_ context: NativeBindingScrollContext?) -> NativeDeviceContextScrollState? {
    guard let context else { return nil }
    let horizontal = context.horizontal.map {
        NativeDeviceContextScrollAxis(value: $0.value, min: $0.minimum, max: $0.maximum)
    }
    let vertical = context.vertical.map {
        NativeDeviceContextScrollAxis(value: $0.value, min: $0.minimum, max: $0.maximum)
    }
    guard horizontal != nil || vertical != nil else { return nil }
    return NativeDeviceContextScrollState(horizontal: horizontal, vertical: vertical)
}

private func validMenuContext(_ menu: NativeDeviceContextMenuRelation,
                              app: NativeDeviceContextApp) -> Bool {
    func validOpener(_ originWindowID: String, _ originWindowFrame: HitRect,
                     _ fingerprint: NativeDeviceContextFingerprint) -> Bool {
        nativeID(originWindowID) && originWindowFrame.valid && fingerprint.valid
            && fingerprint.app == app.bundle_id && fingerprint.app_pid == app.pid
            && fingerprint.window_id == originWindowID
            && [kAXMenuButtonRole, kAXPopUpButtonRole].contains(fingerprint.role)
    }
    switch menu {
    case .openerAnchor(let originWindowID, let originWindowFrame, let openerFingerprint, _):
        return validOpener(originWindowID, originWindowFrame, openerFingerprint)
    case .anchoredMenu(let originWindowID, let originWindowFrame, let openerFingerprint,
                       _, let menuFingerprint, let menuFrame, let drawingWindow):
        return validOpener(originWindowID, originWindowFrame, openerFingerprint)
            && menuFingerprint.valid && menuFingerprint.app == app.bundle_id
            && menuFingerprint.app_pid == app.pid && menuFingerprint.window_id == originWindowID
            && menuFingerprint.role == kAXMenuRole && menuFrame.valid
            && menuFingerprint.frame == menuFrame.rounded
            && drawingWindow.window_id > 0 && drawingWindow.owner_pid == app.pid
            && drawingWindow.layer == 101 && drawingWindow.frame == menuFrame
    }
}

private func validFocusContext(_ focus: NativeDeviceContextFocus,
                               app: NativeDeviceContextApp) -> Bool {
    focus.target_fingerprint.valid && nativeID(focus.origin_window_id)
        && focus.origin_window_id == focus.target_fingerprint.window_id
        && focus.target_fingerprint.app == app.bundle_id
        && focus.target_fingerprint.app_pid == app.pid
        && focus.origin_window_frame.valid && focus.target_frame.valid
        && focus.target_frame.x >= focus.origin_window_frame.x
        && focus.target_frame.y >= focus.origin_window_frame.y
        && focus.target_frame.x + focus.target_frame.width <= focus.origin_window_frame.x + focus.origin_window_frame.width
        && focus.target_frame.y + focus.target_frame.height <= focus.origin_window_frame.y + focus.origin_window_frame.height
        && focus.target_fingerprint.frame == focus.target_frame.rounded
}

/// Strictly checks the current contracts wire shape before Codable decoding.
/// Codable intentionally owns typed values; this layer rejects ignored keys.
func strictNativeDeviceContextWitnessFields(_ raw: Any?) -> Bool {
    func object(_ raw: Any?, keys: Set<String>) -> [String: Any]? {
        guard let value = raw as? [String: Any], Set(value.keys) == keys else { return nil }
        return value
    }
    func optionalString(_ value: Any?) -> Bool {
        value is NSNull || value is String
    }
    func frame(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["x", "y", "width", "height"]) else { return false }
        return ["x", "y", "width", "height"].allSatisfy { value[$0] is NSNumber }
    }
    func sameFrame(_ lhsRaw: Any?, _ rhsRaw: Any?) -> Bool {
        guard let lhs = object(lhsRaw, keys: ["x", "y", "width", "height"]),
              let rhs = object(rhsRaw, keys: ["x", "y", "width", "height"]) else { return false }
        return ["x", "y", "width", "height"].allSatisfy { key in
            guard let left = lhs[key] as? NSNumber, CFGetTypeID(left) == CFNumberGetTypeID(),
                  let right = rhs[key] as? NSNumber, CFGetTypeID(right) == CFNumberGetTypeID() else { return false }
            return left.doubleValue == right.doubleValue
        }
    }
    func fingerprint(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["app", "app_pid", "window_id", "ancestor_roles", "group",
            "role", "subrole", "label", "sibling_index", "frame"]) else { return false }
        return value["app"] is String && value["app_pid"] is NSNumber && value["window_id"] is String
            && value["ancestor_roles"] is [String] && optionalString(value["group"])
            && value["role"] is String && optionalString(value["subrole"])
            && value["label"] is String && value["sibling_index"] is NSNumber
            && value["frame"] is [NSNumber]
    }
    func objectNode(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["role", "subrole", "label", "frame"]) else { return false }
        return value["role"] is String && optionalString(value["subrole"])
            && value["label"] is String && frame(value["frame"])
    }
    func scrollState(_ raw: Any?) -> Bool {
        if raw is NSNull { return true }
        guard let value = object(raw, keys: ["horizontal", "vertical"]) else { return false }
        func axis(_ raw: Any?) -> Bool {
            if raw is NSNull { return true }
            guard let value = object(raw, keys: ["value", "min", "max"]) else { return false }
            return value["value"] is NSNumber
                && (value["min"] is NSNull || value["min"] is NSNumber)
                && (value["max"] is NSNull || value["max"] is NSNumber)
        }
        return axis(value["horizontal"]) && axis(value["vertical"])
    }
    func menu(_ raw: Any?) -> Bool {
        if raw is NSNull { return true }
        guard let value = raw as? [String: Any], let kind = value["kind"] as? String else { return false }
        switch kind {
        case "opener_anchor":
            guard Set(value.keys) == ["kind", "origin_window_id", "origin_window_frame", "opener_fingerprint", "continuity"] else { return false }
            return value["origin_window_id"] is String && frame(value["origin_window_frame"])
                && fingerprint(value["opener_fingerprint"]) && ["direct_read", "observed_lifetime"].contains(value["continuity"] as? String ?? "")
        case "anchored_menu":
            guard Set(value.keys) == ["kind", "origin_window_id", "origin_window_frame", "opener_fingerprint", "continuity", "menu_fingerprint", "menu_frame", "drawing_window"] else { return false }
            guard let drawing = object(value["drawing_window"], keys: ["window_id", "owner_pid", "layer", "frame"]) else { return false }
            return value["origin_window_id"] is String && frame(value["origin_window_frame"])
                && fingerprint(value["opener_fingerprint"]) && ["direct_read", "observed_lifetime"].contains(value["continuity"] as? String ?? "")
                && fingerprint(value["menu_fingerprint"]) && frame(value["menu_frame"])
                && drawing["window_id"] is NSNumber && drawing["owner_pid"] is NSNumber
                && drawing["layer"] is NSNumber && frame(drawing["frame"])
        default: return false
        }
    }
    func focus(_ raw: Any?) -> Bool {
        if raw is NSNull { return true }
        guard let value = object(raw, keys: ["origin_window_id", "origin_window_frame", "target_fingerprint",
            "target_frame", "relation", "composition"]) else { return false }
        return value["origin_window_id"] is String && frame(value["origin_window_frame"])
            && fingerprint(value["target_fingerprint"]) && frame(value["target_frame"])
            && (value["relation"] as? String) == "exact_control"
            && ["unknown", "not_applicable", "marked", "unmarked"].contains(value["composition"] as? String ?? "")
    }
    func smallIndex(_ raw: Any?, maximum: Int) -> Int? {
        guard let number = raw as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID(),
              number.doubleValue.isFinite,
              number.doubleValue.rounded(.towardZero) == number.doubleValue,
              number.doubleValue >= 0, number.doubleValue <= Double(maximum) else { return nil }
        return Int(number.doubleValue)
    }
    func unsignedInteger(_ raw: Any?, allowZero: Bool = false) -> UInt64? {
        guard let number = raw as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID(),
              number.doubleValue.isFinite, number.doubleValue >= 0,
              number.doubleValue.rounded(.towardZero) == number.doubleValue,
              let value = UInt64(number.stringValue), allowZero || value > 0,
              Double(value) == number.doubleValue else { return nil }
        return value
    }
    func tableColumn(_ raw: Any?) -> Bool {
        if raw is NSNull { return true }
        guard let value = object(raw, keys: ["table_ancestry_index", "ordered_ax_indices", "target_ax_index"]),
              let ancestryIndex = smallIndex(value["table_ancestry_index"], maximum: 31),
              let values = value["ordered_ax_indices"] as? [Any], !values.isEmpty, values.count <= 64,
              let targetIndex = smallIndex(value["target_ax_index"], maximum: 63) else { return false }
        let indices = values.compactMap { smallIndex($0, maximum: 63) }
        return indices.count == values.count && Set(indices).count == indices.count
            && indices.filter { $0 == targetIndex }.count == 1 && ancestryIndex <= 31
    }
    func jsonBoolean(_ raw: Any?) -> Bool {
        guard let number = raw as? NSNumber else { return false }
        return CFGetTypeID(number) == CFBooleanGetTypeID()
    }
    func axEnabledObservation(_ raw: Any?) -> Bool {
        guard let value = raw as? String else { return false }
        return ["enabled", "disabled", "attribute_unsupported", "unavailable"].contains(value)
    }
    func boundedFloat(_ raw: Any?, maximum: Double) -> Bool {
        guard let number = raw as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID() else { return false }
        return number.doubleValue.isFinite && abs(number.doubleValue) <= maximum
    }
    func uiValue(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["type", "value"]),
              let kind = value["type"] as? String else { return false }
        switch kind {
        case "bool": return jsonBoolean(value["value"])
        case "text":
            guard let text = value["value"] as? String else { return false }
            return !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && text.utf8.count <= 2_048 && !text.unicodeScalars.contains { $0.properties.generalCategory == .control }
        case "number": return boundedFloat(value["value"], maximum: 1.0e12)
        default: return false
        }
    }
    func rowColumn(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["ax_index", "header", "frame"]),
              smallIndex(value["ax_index"], maximum: 63) != nil,
              let header = value["header"] as? String,
              let index = value["ax_index"] as? NSNumber,
              header.utf8.count <= 256,
              !header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !header.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }),
              frame(value["frame"]) else { return false }
        return index.doubleValue.rounded(.towardZero) == index.doubleValue
    }
    func selectorColumn(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["ax_index", "header", "expected_value"]),
              smallIndex(value["ax_index"], maximum: 63) != nil,
              let header = value["header"] as? String,
              header.utf8.count <= 256,
              !header.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !header.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return false }
        return uiValue(value["expected_value"])
    }
    func visibleRow(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["ax_index", "frame", "selector_values"]),
              smallIndex(value["ax_index"], maximum: Int(UInt32.max)) != nil,
              frame(value["frame"]),
              let values = value["selector_values"] as? [Any],
              !values.isEmpty, values.count <= 32 else { return false }
        return values.allSatisfy(uiValue)
    }
    func point(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: ["x", "y"]) else { return false }
        return boundedFloat(value["x"], maximum: 100_000)
            && boundedFloat(value["y"], maximum: 100_000)
    }
    func booleanFact(_ raw: Any?) -> Bool {
        guard let value = raw as? [String: Any], let state = value["state"] as? String else { return false }
        switch state {
        case "known":
            return Set(value.keys) == ["state", "value"] && jsonBoolean(value["value"])
        case "absent", "unavailable":
            return Set(value.keys) == ["state"]
        default:
            return false
        }
    }
    func backgroundAX(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: NativeDeviceContextWitness.backgroundAxV1Keys),
              let app = object(value["app"], keys: ["bundle_id", "pid"]),
              let window = object(value["window"], keys: ["window_id", "title", "frame", "kind", "modal", "owner_bundle_id"]),
              let root = object(value["root"], keys: ["role", "subrole", "label", "frame"]),
              let target = object(value["target"], keys: ["object", "group", "scroll"]),
              let source = object(value["source"], keys: ["window", "hidden", "minimized", "target_context_hash"]),
              let sourceWindow = object(source["window"], keys: ["source_session_id", "session_epoch", "process",
                  "origin_window_id", "origin_window_frame", "verified_source_sequence", "verified_observed_ms"]),
              let process = object(sourceWindow["process"], keys: ["pid", "start_seconds", "start_micros"]),
              let ancestry = value["ancestry"] as? [Any],
              let siblings = value["siblings"] as? [Any],
              let actionNames = value["target_action_names"] as? [Any],
              let appBundle = app["bundle_id"] as? String,
              let appPID = unsignedInteger(app["pid"]), appPID <= UInt64(Int32.max),
              let windowID = window["window_id"] as? String,
              let sourceWindowID = sourceWindow["origin_window_id"] as? String,
              let sourceHash = source["target_context_hash"] as? String,
              let witnessHash = value["target_context_hash"] as? String,
              let title = window["title"], optionalString(title),
              let ownerBundle = window["owner_bundle_id"] as? String,
              let sessionID = sourceWindow["source_session_id"] as? String,
              let sessionEpoch = unsignedInteger(sourceWindow["session_epoch"]),
              let sourcePID = unsignedInteger(process["pid"]),
              let startSeconds = unsignedInteger(process["start_seconds"]),
              let startMicros = unsignedInteger(process["start_micros"], allowZero: true),
              let sequence = unsignedInteger(sourceWindow["verified_source_sequence"]),
              unsignedInteger(sourceWindow["verified_observed_ms"], allowZero: true) != nil,
              let parentIndex = smallIndex(value["sibling_parent_ancestry_index"], maximum: 31),
              let targetIndex = smallIndex(value["target_sibling_index"], maximum: 255),
              (value["nearest_labelled_group_ancestry_index"] is NSNull
                  || smallIndex(value["nearest_labelled_group_ancestry_index"], maximum: 31) != nil) else { return false }
        guard nativeID(appBundle), appPID > 0, sourcePID == appPID,
              startSeconds > 0, startMicros < 1_000_000,
              nativeID(windowID), windowID.hasPrefix("window-"),
              UInt32(windowID.dropFirst(7)).map({ $0 > 0 }) == true,
              sourceWindowID == windowID, ownerBundle == appBundle,
              (window["kind"] as? String) == "window",
              jsonBoolean(window["modal"]), (window["modal"] as? Bool) == false,
              frame(window["frame"]), frame(sourceWindow["origin_window_frame"]),
              sameFrame(window["frame"], sourceWindow["origin_window_frame"]),
              objectNode(root), objectNode(target["object"]),
              (root["role"] as? String) == kAXWindowRole,
              (root["subrole"] as? String) == kAXStandardWindowSubrole,
              sameFrame(root["frame"], window["frame"]),
              ancestry.count >= 1, ancestry.count <= 32,
              siblings.count >= 1, siblings.count <= 256,
              ancestry.allSatisfy(objectNode), siblings.allSatisfy(objectNode),
              parentIndex == ancestry.count - 1, targetIndex < siblings.count,
              optionalString(target["group"]), target["scroll"] is NSNull,
              jsonBoolean(value["target_enabled"]), (value["target_enabled"] as? Bool) == true,
              actionNames.count >= 1, actionNames.count <= 32,
              actionNames.allSatisfy({ $0 is String }),
              booleanFact(source["hidden"]), booleanFact(source["minimized"]),
              nativeID(sessionID), sessionEpoch > 0, sequence > 0,
              sourceHash == witnessHash,
              sourceHash.utf8.count == 68,
              sourceHash.hasPrefix("tc1:"),
              sourceHash.dropFirst(4).allSatisfy({ $0.isHexDigit && !$0.isUppercase }) else { return false }
        return true
    }
    func tableViewport(_ raw: Any?) -> Bool {
        guard let value = object(raw, keys: NativeDeviceContextWitness.tableViewportV1Keys),
              let app = object(value["app"], keys: ["bundle_id", "pid"]),
              let window = object(value["window"], keys: ["window_id", "title", "frame", "kind", "modal", "owner_bundle_id"]),
              let target = object(value["target"], keys: ["object", "group", "scroll"]),
              let scrollPath = value["scroll_path"] as? [Any],
              let orderedColumns = value["ordered_columns"] as? [Any],
              let selectorColumns = value["selector_columns"] as? [Any],
              let visibleRows = value["visible_rows"] as? [Any] else { return false }
        guard app["bundle_id"] is String && app["pid"] is NSNumber,
              window["window_id"] is String && optionalString(window["title"])
                && frame(window["frame"])
                && ["window", "sheet"].contains(window["kind"] as? String ?? "")
                && jsonBoolean(window["modal"]) && window["owner_bundle_id"] is String,
              objectNode(value["table"]), objectNode(target["object"]),
              optionalString(target["group"]), target["scroll"] is NSNull,
              let targetObject = target["object"] as? [String: Any],
              targetObject["role"] as? String == kAXRowRole,
              targetObject["subrole"] as? String == "AXTableRow",
              let tableObject = value["table"] as? [String: Any],
              tableObject["role"] as? String == kAXTableRole,
              frame(value["viewport_frame"]), point(value["hit_point"]),
              !scrollPath.isEmpty, scrollPath.count <= 8, scrollPath.allSatisfy(objectNode),
              !orderedColumns.isEmpty, orderedColumns.count <= 64, orderedColumns.allSatisfy(rowColumn),
              !selectorColumns.isEmpty, selectorColumns.count <= 32, selectorColumns.allSatisfy(selectorColumn),
              !visibleRows.isEmpty, visibleRows.count <= 256, visibleRows.allSatisfy(visibleRow),
              smallIndex(value["target_row_ax_index"], maximum: Int(UInt32.max)) != nil,
              axEnabledObservation(value["table_ax_enabled"]),
              axEnabledObservation(value["target_ax_enabled"]),
              jsonBoolean(value["target_selected"]), jsonBoolean(value["target_ax_selected_settable"]) else { return false }
        return true
    }

    guard let value = raw as? [String: Any], let scope = value["scope"] as? String,
          JSONSerialization.isValidJSONObject(value),
          let encoded = try? JSONSerialization.data(withJSONObject: value), encoded.count <= 4096 else { return false }
    switch scope {
    case "unsupported":
        guard Set(value.keys) == ["scope", "reason"] else { return false }
        return (value["reason"] as? String) == "table_viewport"
    case "table_viewport_v1":
        return tableViewport(value)
    case "window_root_v1":
        guard Set(value.keys) == NativeDeviceContextWitness.windowRootV1Keys,
              let app = object(value["app"], keys: ["bundle_id", "pid"]),
              let window = object(value["window"], keys: ["window_id", "title", "frame", "kind", "modal", "owner_bundle_id"]),
              let root = object(value["root"], keys: ["role", "subrole", "label", "frame"]),
              let source = object(value["source"], keys: ["source_session_id", "session_epoch", "process",
                  "origin_window_id", "origin_window_frame", "verified_source_sequence", "verified_observed_ms"]),
              let process = object(source["process"], keys: ["pid", "start_seconds", "start_micros"]),
              let appBundle = app["bundle_id"] as? String,
              let appPID = unsignedInteger(app["pid"]), appPID <= UInt64(Int32.max),
              let windowID = window["window_id"] as? String,
              let title = window["title"] as? String,
              let ownerBundle = window["owner_bundle_id"] as? String,
              let processPID = unsignedInteger(process["pid"]),
              let startSeconds = unsignedInteger(process["start_seconds"]),
              let startMicros = unsignedInteger(process["start_micros"], allowZero: true),
              let sessionID = source["source_session_id"] as? String,
              let epoch = unsignedInteger(source["session_epoch"]),
              let originWindowID = source["origin_window_id"] as? String,
              let sequence = unsignedInteger(source["verified_source_sequence"]),
              unsignedInteger(source["verified_observed_ms"], allowZero: true) != nil else { return false }
        return nativeID(appBundle) && appPID == processPID && processPID <= UInt64(Int32.max)
            && startSeconds > 0 && startMicros < 1_000_000
            && nativeID(windowID) && windowID.hasPrefix("window-")
            && UInt32(windowID.dropFirst(7)).map({ $0 > 0 }) == true
            && title.utf8.count <= 512
            && !title.unicodeScalars.contains { $0.properties.generalCategory == .control }
            && bindingLabel(title) == title && ownerBundle == appBundle
            && (window["kind"] as? String) == "window" && jsonBoolean(window["modal"])
            && (window["modal"] as? Bool) == false && frame(window["frame"])
            && (root["role"] as? String) == kAXWindowRole
            && (root["subrole"] as? String) == kAXStandardWindowSubrole
            && (root["label"] as? String) == title && frame(root["frame"])
            && sameFrame(root["frame"], window["frame"])
            && nativeID(sessionID) && epoch > 0 && nativeID(originWindowID)
            && originWindowID == windowID && frame(source["origin_window_frame"])
            && sameFrame(source["origin_window_frame"], window["frame"])
            && sequence > 0
    case "background_ax_v1":
        return backgroundAX(value)
    case "whole_structure_v1":
        guard Set(value.keys) == NativeDeviceContextWitness.wholeStructureKeys,
              let app = object(value["app"], keys: ["bundle_id", "pid"]),
              let window = object(value["window"], keys: ["window_id", "title", "frame", "kind", "modal", "owner_bundle_id"]),
              let target = object(value["target"], keys: ["object", "group", "scroll"]),
              let ancestry = value["ancestry"] as? [Any], let siblings = value["siblings"] as? [Any],
              let scrollAncestors = value["scroll_ancestors"] as? [Any] else { return false }
        guard app["bundle_id"] is String && app["pid"] is NSNumber,
              optionalString(value["active_layer"]),
              window["window_id"] is String && optionalString(window["title"]) && frame(window["frame"])
                && ["window", "sheet", "dialog", "popover", "menu"].contains(window["kind"] as? String ?? "")
                && window["modal"] is NSNumber && window["owner_bundle_id"] is String,
              objectNode(target["object"]), optionalString(target["group"]), scrollState(target["scroll"]),
              value["sibling_parent_ancestry_index"] is NSNull || value["sibling_parent_ancestry_index"] is NSNumber,
              value["target_sibling_index"] is NSNumber,
              value["nearest_labelled_group_ancestry_index"] is NSNull || value["nearest_labelled_group_ancestry_index"] is NSNumber,
              ancestry.allSatisfy(objectNode), siblings.allSatisfy(objectNode),
              tableColumn(value["table_column"]) else { return false }
        let targetObject = target["object"] as? [String: Any]
        let tableRoles = [kAXTableRole, "AXOutline"]
        let tableRelated = ancestry.contains { rawNode in
            guard let node = rawNode as? [String: Any], let role = node["role"] as? String else { return false }
            return tableRoles.contains(role)
        } || tableRoles.contains(targetObject?["role"] as? String ?? "")
            || (targetObject?["role"] as? String == kAXButtonRole
                && targetObject?["subrole"] as? String == "AXSortButton")
        if value["table_column"] is NSNull {
            guard !tableRelated else { return false }
        } else {
            guard let table = value["table_column"] as? [String: Any],
                  let tableIndex = smallIndex(table["table_ancestry_index"], maximum: 31),
                  let targetRole = targetObject?["role"] as? String,
                  let targetSubrole = targetObject?["subrole"] as? String,
                  targetRole == kAXButtonRole, targetSubrole == "AXSortButton",
                  value["menu"] is NSNull, value["focus"] is NSNull,
                  !ancestry.contains(where: { rawNode in
                      (rawNode as? [String: Any])?["role"] as? String == "AXOutline"
                  }),
                  ancestry.indices.filter({
                      guard let node = ancestry[$0] as? [String: Any] else { return false }
                      return node["role"] as? String == kAXTableRole
                  }) == [tableIndex] else { return false }
        }
        for item in scrollAncestors {
            guard let scroll = object(item, keys: ["ancestry_index", "state"]),
                  scroll["ancestry_index"] is NSNumber, scrollState(scroll["state"]) else { return false }
        }
        return menu(value["menu"]) && focus(value["focus"])
    default:
        return false
    }
}
