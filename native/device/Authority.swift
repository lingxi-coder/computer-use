import Foundation
import ApplicationServices

struct NativeSurface: Codable, Sendable, Equatable { let id: String; let epoch: UInt64 }
struct NativePermit: Codable, Sendable, Equatable { let id: String; let generation: UInt64 }

func nativeID(_ value: String) -> Bool {
    !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && value.utf8.count <= 256 &&
        !value.unicodeScalars.contains { $0.properties.generalCategory == .control }
}

func nativeBindingContextDigest(_ value: String) -> Bool {
    let bytes = Array(value.utf8)
    return bytes.count == 68 && Array(bytes.prefix(4)) == Array("bc1:".utf8) &&
        bytes.dropFirst(4).allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102) }
}

func nativeTargetContextHash(_ value: String) -> Bool {
    let bytes = Array(value.utf8)
    return bytes.count == 68 && Array(bytes.prefix(4)) == Array("tc1:".utf8) &&
        bytes.dropFirst(4).allSatisfy { ($0 >= 48 && $0 <= 57) || ($0 >= 97 && $0 <= 102) }
}

enum NativeObjectIdentityContinuity: String, Codable, Sendable, Equatable {
    case notificationCovered = "notification_covered"
    case readScoped = "read_scoped"
}

enum NativeBindingProofScope: Equatable, Sendable, Codable {
    case wholeStructure
    case tableViewport(table: String, scrollContainer: String, viewportFrame: HitRect,
                       visibleRowsComplete: Bool, columnsComplete: Bool)
    case windowRoot(tableScope: NativeDeviceContextRowTableScope)
    case backgroundAX(targetContextHash: String)

    private enum CodingKeys: String, CodingKey {
        case kind, table_instance_ref, scroll_container_instance_ref, viewport_frame
        case visible_rows_complete, columns_complete, table_scope, target_context_hash
    }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .kind) {
        case "whole_structure":
            self = .wholeStructure
        case "table_viewport":
            self = .tableViewport(
                table: try values.decode(String.self, forKey: .table_instance_ref),
                scrollContainer: try values.decode(String.self, forKey: .scroll_container_instance_ref),
                viewportFrame: try values.decode(HitRect.self, forKey: .viewport_frame),
                visibleRowsComplete: try values.decode(Bool.self, forKey: .visible_rows_complete),
                columnsComplete: try values.decode(Bool.self, forKey: .columns_complete))
        case "window_root":
            self = .windowRoot(tableScope: try values.decode(NativeDeviceContextRowTableScope.self,
                forKey: .table_scope))
        case "background_ax":
            self = .backgroundAX(targetContextHash: try values.decode(String.self, forKey: .target_context_hash))
        default:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values,
                debugDescription: "unknown binding proof scope")
        }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .wholeStructure:
            try values.encode("whole_structure", forKey: .kind)
        case .tableViewport(let table, let scrollContainer, let viewportFrame,
                            let visibleRowsComplete, let columnsComplete):
            try values.encode("table_viewport", forKey: .kind)
            try values.encode(table, forKey: .table_instance_ref)
            try values.encode(scrollContainer, forKey: .scroll_container_instance_ref)
            try values.encode(viewportFrame, forKey: .viewport_frame)
            try values.encode(visibleRowsComplete, forKey: .visible_rows_complete)
            try values.encode(columnsComplete, forKey: .columns_complete)
        case .windowRoot(let tableScope):
            try values.encode("window_root", forKey: .kind)
            try values.encode(tableScope, forKey: .table_scope)
        case .backgroundAX(let targetContextHash):
            try values.encode("background_ax", forKey: .kind)
            try values.encode(targetContextHash, forKey: .target_context_hash)
        }
    }

    var valid: Bool {
        switch self {
        case .wholeStructure:
            return true
        case .tableViewport(let table, let scrollContainer, let viewportFrame,
                            let visibleRowsComplete, let columnsComplete):
            return nativeID(table) && nativeID(scrollContainer) && table != scrollContainer && viewportFrame.valid &&
                visibleRowsComplete && columnsComplete
        case .windowRoot(let tableScope):
            return tableScope.valid
        case .backgroundAX(let targetContextHash):
            return nativeTargetContextHash(targetContextHash)
        }
    }

    /// Generic Native actions still require a whole-structure proof. Row
    /// selection uses the separate value-only TableViewportV1 comparator.
    var supportedForAtomicInput: Bool {
        if case .wholeStructure = self { return true }
        return false
    }

    func supportsRowSelection(witness: NativeDeviceContextWitness) -> Bool {
        guard case let .tableViewport(table, scrollContainer, viewportFrame, visibleRowsComplete, columnsComplete) = self,
              let context = witness.tableViewport else { return false }
        return nativeID(table) && nativeID(scrollContainer) && table != scrollContainer
            && viewportFrame == context.viewport_frame
            && visibleRowsComplete && columnsComplete && context.valid
    }

    /// Window activation is a separate purpose-bound lane. The frozen row
    /// scope is validated here, while its Device witness intentionally carries
    /// only value-only root and Source provenance facts.
    func supportsWindowActivation(witness: NativeDeviceContextWitness) -> Bool {
        switch (self, witness) {
        case (.wholeStructure, .wholeStructureV1):
            return witness.valid
        case (.windowRoot(let tableScope), .windowRootV1(let context)):
            return tableScope.valid && context.valid
        default:
            return false
        }
    }

    func supportsBackgroundAXPress(witness: NativeDeviceContextWitness) -> Bool {
        guard case let .backgroundAX(targetContextHash) = self,
              case let .backgroundAxV1(context) = witness else { return false }
        return nativeTargetContextHash(targetContextHash)
            && targetContextHash == context.target_context_hash
            && context.valid
    }
}

private func backgroundTargetContextHash(_ scope: NativeBindingProofScope) -> String? {
    guard case .backgroundAX(let hash) = scope else { return nil }
    return hash
}

/// Keeps TargetObject and the background purpose together at the native wire
/// boundary. Missing optional mode fields remain compatible with older
/// foreground packets, but no packet can claim TargetObject outside this lane.
func nativeRequestExecutionModeIsValid(
    executionDomain: String?,
    purpose: String?,
    action: String,
    coordinateSpace: String?,
    contextWitness: NativeDeviceContextWitness,
    originalScope: NativeBindingProofScope,
    currentScope: NativeBindingProofScope
) -> Bool {
    let witnessHash = contextWitness.backgroundAXV1?.target_context_hash
    let originalHash = backgroundTargetContextHash(originalScope)
    let currentHash = backgroundTargetContextHash(currentScope)
    let backgroundClaim = executionDomain == "background_ax" || purpose == "background_ax_action"
        || coordinateSpace == "target_object" || witnessHash != nil
        || originalHash != nil || currentHash != nil
    if backgroundClaim {
        guard executionDomain == "background_ax", purpose == "background_ax_action",
              action == "ax_press", coordinateSpace == "target_object",
              let witnessHash, let originalHash, let currentHash,
              case .backgroundAxV1(let context) = contextWitness else { return false }
        return nativeTargetContextHash(witnessHash)
            && witnessHash == originalHash && witnessHash == currentHash
            && witnessHash == context.source.target_context_hash
    }
    guard executionDomain.map({ ["foreground_native", "simulated"].contains($0) }) ?? true,
          purpose.map({ ["target_input", "window_activation", "menu_selection", "focused_control", "row_selection"].contains($0) }) ?? true,
          coordinateSpace == nil || coordinateSpace == "display_points" else { return false }
    return true
}

struct NativeBindingContinuityProof: Codable, Sendable, Equatable {
    let kind: NativeObjectIdentityContinuity
    let source_sequence: UInt64
    let context_digest: String
    let scope: NativeBindingProofScope

    static let keys: Set<String> = ["kind", "source_sequence", "context_digest", "scope"]
    var valid: Bool {
        // The digest is an opaque Source commitment. The native worker pins it
        // across proofs; AtomicHitTest independently rereads the live AX path
        // but does not recompute this source digest.
        source_sequence > 0 && nativeBindingContextDigest(context_digest) &&
            scope.valid
    }
    func sameBindingContext(_ other: Self) -> Bool {
        kind == other.kind && context_digest == other.context_digest && scope == other.scope
    }
}

struct NativeSourceProof: Codable, Sendable, Equatable {
    let binding: String, instance: String, snapshot_id: String
    let environment_revision: UInt64, layer_revision: UInt64
    let fingerprint: NativeBindingFingerprint
    let display_frame: HitRect
    let continuity: NativeBindingContinuityProof
    let menu_scope: NativeMenuBindingScope?
    let focus_scope: NativeFocusedControlScope?
    static let keys: Set<String> = ["binding", "instance", "snapshot_id", "environment_revision", "layer_revision", "fingerprint", "display_frame", "continuity"]
    var valid: Bool {
        [binding, instance, snapshot_id].allSatisfy(nativeID) && environment_revision > 0 && layer_revision > 0 &&
            fingerprint.valid && display_frame.valid && fingerprint.frame == display_frame.rounded && continuity.valid &&
            !(menu_scope != nil && focus_scope != nil) && (focus_scope?.allows(self) ?? true) &&
            (menu_scope.map { $0.allows(self) } ?? (fingerprint.role != kAXMenuItemRole && !fingerprint.ancestor_roles.contains(where: { [kAXMenuRole, kAXMenuBarRole].contains($0) })))
    }
    func sameInstance(_ other: Self) -> Bool {
        binding == other.binding && instance == other.instance && layer_revision == other.layer_revision &&
            fingerprint == other.fingerprint && display_frame == other.display_frame &&
            continuity.sameBindingContext(other.continuity) &&
            (continuity.kind == .notificationCovered ||
                (continuity.source_sequence == other.continuity.source_sequence &&
                 snapshot_id == other.snapshot_id && environment_revision == other.environment_revision)) &&
            (menu_scope == nil && other.menu_scope == nil || menu_scope.map { scope in other.menu_scope.map(scope.sameIdentity) ?? false } == true) &&
            (focus_scope == nil && other.focus_scope == nil || focus_scope.map { scope in other.focus_scope.map(scope.sameIdentity) ?? false } == true)
    }
}

struct NativeMouseModifiers: Decodable, Sendable, Equatable {
    let shift: Bool, control: Bool, option: Bool, command: Bool
    var flags: CGEventFlags {
        var flags: CGEventFlags = []
        if shift { flags.insert(.maskShift) }
        if control { flags.insert(.maskControl) }
        if option { flags.insert(.maskAlternate) }
        if command { flags.insert(.maskCommand) }
        return flags
    }
}
struct NativeMouse: Decodable, Sendable, Equatable {
    let button: String
    let modifiers: NativeMouseModifiers
    var valid: Bool { ["left", "right", "middle"].contains(button) }
}

struct NativeScroll: Decodable, Sendable, Equatable {
    let dx: Int32, dy: Int32
    var valid: Bool { (-120...120).contains(dx) && (-120...120).contains(dy) && (dx != 0 || dy != 0) }
}

/// A parsed immutable packet is shared by admission, the serial AX worker and
/// submission. Keeping its two complete Source proofs on the heap prevents
/// value copies in nested validation from exhausting the default worker stack.
/// Sendable is compiler checked; this object contains no AX references.
final class NativeProbe: Decodable, Sendable, Equatable {
    let request_id: String, intent_id: String, task_id: String
    let attempt_id: String?
    let task_revision: UInt64, root_epoch: UInt64
    let permit: NativePermit
    let stop_seq: UInt64, guard_revision: UInt64
    let clock_domain: String, surface: NativeSurface
    let execution_domain: String?
    let purpose: String?
    let evidence_id: String, intent_evidence_id: String
    let source_sequence: UInt64, observed_ms: UInt64, valid_until_ms: UInt64, command_end_ms: UInt64
    let started_ms: UInt64, duration_ms: UInt64, latest_start_ms: UInt64
    let original_proof: NativeSourceProof, current_proof: NativeSourceProof
    let context_witness: NativeDeviceContextWitness
    let binding_digest: String, dependencies: [String: UInt64]
    let action: String, x: Double, y: Double
    let coordinate_space: String?
    let value: String?
    let mouse: NativeMouse?
    let scroll: NativeScroll?
    let key: NativeKey?
    static func == (lhs: NativeProbe, rhs: NativeProbe) -> Bool {
        if lhs === rhs { return true }
        guard lhs.request_id == rhs.request_id, lhs.intent_id == rhs.intent_id, lhs.task_id == rhs.task_id,
              lhs.attempt_id == rhs.attempt_id, lhs.task_revision == rhs.task_revision, lhs.root_epoch == rhs.root_epoch,
              lhs.permit == rhs.permit, lhs.stop_seq == rhs.stop_seq, lhs.guard_revision == rhs.guard_revision,
              lhs.clock_domain == rhs.clock_domain, lhs.surface == rhs.surface,
              lhs.execution_domain == rhs.execution_domain, lhs.purpose == rhs.purpose,
              lhs.evidence_id == rhs.evidence_id, lhs.intent_evidence_id == rhs.intent_evidence_id,
              lhs.source_sequence == rhs.source_sequence, lhs.observed_ms == rhs.observed_ms,
              lhs.valid_until_ms == rhs.valid_until_ms, lhs.command_end_ms == rhs.command_end_ms,
              lhs.started_ms == rhs.started_ms, lhs.duration_ms == rhs.duration_ms, lhs.latest_start_ms == rhs.latest_start_ms,
              lhs.original_proof == rhs.original_proof, lhs.current_proof == rhs.current_proof,
              lhs.context_witness == rhs.context_witness,
              lhs.binding_digest == rhs.binding_digest, lhs.dependencies == rhs.dependencies,
              lhs.action == rhs.action, lhs.x == rhs.x, lhs.y == rhs.y,
              lhs.coordinate_space == rhs.coordinate_space,
              lhs.value == rhs.value, lhs.mouse == rhs.mouse, lhs.scroll == rhs.scroll, lhs.key == rhs.key else { return false }
        return true
    }
    var claimsBackgroundAX: Bool {
        if execution_domain == "background_ax" || purpose == "background_ax_action"
            || coordinate_space == "target_object" || context_witness.backgroundAXV1 != nil { return true }
        if case .backgroundAX = original_proof.continuity.scope { return true }
        if case .backgroundAX = current_proof.continuity.scope { return true }
        return false
    }
    static let keys: Set<String> = ["request_id", "intent_id", "task_id", "task_revision", "root_epoch", "permit",
        "stop_seq", "guard_revision", "clock_domain", "surface", "evidence_id", "intent_evidence_id", "source_sequence",
        "observed_ms", "valid_until_ms", "command_end_ms", "started_ms", "duration_ms", "latest_start_ms", "original_proof", "current_proof", "context_witness", "binding_digest", "dependencies", "action", "x", "y"]
    var hit: HitRequest? {
        let fp = original_proof.fingerprint
        guard valid, !claimsBackgroundAX,
              let window = UInt32(fp.window_id.dropFirst(7)) else { return nil }
        return HitRequest(request_id: request_id, pid: Int32(fp.app_pid), bundle_id: fp.app,
            window_id: window, x: x, y: y, frame: original_proof.display_frame,
            binding: fp, binding_digest: binding_digest, contextWitness: context_witness)
    }
    var valid: Bool {
        let fp = original_proof.fingerprint
        let rowSelection = action == "ax_set_selected_true"
        let backgroundAX = claimsBackgroundAX
        let executionModeValid = nativeRequestExecutionModeIsValid(
            executionDomain: execution_domain, purpose: purpose, action: action,
            coordinateSpace: coordinate_space, contextWitness: context_witness,
            originalScope: original_proof.continuity.scope,
            currentScope: current_proof.continuity.scope)
        let sourceScopeValid: Bool
        if backgroundAX {
            sourceScopeValid = original_proof.continuity.scope.supportsBackgroundAXPress(witness: context_witness)
                && current_proof.continuity.scope.supportsBackgroundAXPress(witness: context_witness)
        } else if action == "activate_window" {
            sourceScopeValid = original_proof.continuity.scope.supportsWindowActivation(witness: context_witness)
                && current_proof.continuity.scope.supportsWindowActivation(witness: context_witness)
        } else if rowSelection {
            sourceScopeValid = original_proof.continuity.scope.supportsRowSelection(witness: context_witness)
                && current_proof.continuity.scope.supportsRowSelection(witness: context_witness)
        } else {
            sourceScopeValid = original_proof.continuity.scope.supportedForAtomicInput
                && current_proof.continuity.scope.supportedForAtomicInput
        }
        let windowRootSourceValid = context_witness.windowRootV1.map {
            $0.matchesSourceProvenance(sequence: source_sequence, observedMS: observed_ms)
        } ?? true
        let backgroundSourceValid = context_witness.backgroundAXV1.map {
            $0.source.matchesSourceProvenance(sequence: source_sequence, observedMS: observed_ms)
                && $0.source.window.session_epoch > 0
                && $0.source.window.process.pid == fp.app_pid
                && $0.source.valid(bundleID: fp.app, pid: fp.app_pid,
                    windowID: fp.window_id, frame: $0.window.frame)
                && $0.target_context_hash == $0.source.target_context_hash
        } ?? true
        let backgroundRequestValid: Bool
        if backgroundAX {
            let maxExpiry = observed_ms.addingReportingOverflow(2_000)
            backgroundRequestValid = execution_domain == "background_ax"
                && purpose == "background_ax_action"
                && action == "ax_press"
                && context_witness.backgroundAXV1 != nil
                && original_proof.continuity.scope == current_proof.continuity.scope
                && original_proof.menu_scope == nil && current_proof.menu_scope == nil
                && original_proof.focus_scope == nil && current_proof.focus_scope == nil
                && maxExpiry.overflow == false && valid_until_ms <= maxExpiry.partialValue
                && Set(dependencies.keys) == ["authorization", "background_scope", "binding", "geometry", "window"]
                && x == 0 && y == 0
        } else {
            backgroundRequestValid = execution_domain != "background_ax"
                && purpose != "background_ax_action"
                && context_witness.backgroundAXV1 == nil
        }
        return nativeID(request_id) && request_id.utf8.count <= 128 && (attempt_id == nil || nativeID(attempt_id!)) &&
            [intent_id, task_id, evidence_id, intent_evidence_id, permit.id, surface.id].allSatisfy(nativeID) &&
            task_revision > 0 && root_epoch > 0 && permit.generation > 0 && surface.epoch > 0 &&
            original_proof.valid && current_proof.valid && original_proof.sameInstance(current_proof) &&
            context_witness.valid && context_witness.supportsNativeInput(action: action)
                && context_witness.supportsAction(action) &&
            context_witness.matchesBinding(current_proof.fingerprint, displayFrame: current_proof.display_frame) &&
            context_witness.matchesScopes(menuScope: current_proof.menu_scope, focusScope: current_proof.focus_scope) &&
            current_proof.environment_revision >= original_proof.environment_revision &&
            sourceScopeValid && windowRootSourceValid && backgroundSourceValid && backgroundRequestValid
                && executionModeValid && (!rowSelection || original_proof == current_proof) &&
            current_proof.continuity.source_sequence == source_sequence &&
            ((original_proof.continuity.kind == .notificationCovered &&
              current_proof.continuity.kind == .notificationCovered &&
              current_proof.continuity.source_sequence >= original_proof.continuity.source_sequence) ||
                (original_proof.continuity.kind == .readScoped && current_proof.continuity.kind == .readScoped &&
                 original_proof.continuity.source_sequence == source_sequence &&
                 current_proof.continuity == original_proof.continuity &&
                 current_proof.snapshot_id == original_proof.snapshot_id &&
                 current_proof.environment_revision == original_proof.environment_revision &&
                 current_proof == original_proof)) &&
            binding_digest == fp.digest && fp.app_pid <= UInt32(Int32.max) &&
            fp.window_id.hasPrefix("window-") && UInt32(fp.window_id.dropFirst(7)).map { $0 > 0 } == true &&
            dependencies.count > 0 && dependencies.count <= 16 &&
            dependencies.allSatisfy { nativeID($0.key) && $0.key.utf8.count <= 64 && $0.value > 0 } &&
            (backgroundAX || (action == "activate_window" ? Set(dependencies.keys) == ["authorization", "window"] : ["focus", "layer", "binding", "authorization", "geometry"].allSatisfy { dependencies[$0] != nil })) &&
            validMenuProvenance && validFocusProvenance && validAction && x.isFinite && y.isFinite &&
            (backgroundAX ? x == 0 && y == 0 : original_proof.display_frame.contains(x: x, y: y)) &&
            valid_until_ms <= command_end_ms && observed_ms < valid_until_ms && duration_ms > 0 && duration_ms <= 1500 &&
            started_ms <= latest_start_ms && started_ms <= UInt64.max - duration_ms && command_end_ms == started_ms + duration_ms
    }
    var validAction: Bool {
        if claimsBackgroundAX {
            return execution_domain == "background_ax" && purpose == "background_ax_action"
                && action == "ax_press" && value == nil && mouse == nil && scroll == nil && key == nil
                && original_proof.menu_scope == nil && current_proof.menu_scope == nil
                && original_proof.focus_scope == nil && current_proof.focus_scope == nil
                && x == 0 && y == 0
        }
        if action == "ax_set_selected_true" {
            return context_witness.supportsAction(action)
                && original_proof.menu_scope == nil && current_proof.menu_scope == nil
                && original_proof.focus_scope == nil && current_proof.focus_scope == nil
                && value == nil && mouse == nil && scroll == nil && key == nil
                && original_proof == current_proof
        }
        if let scope = original_proof.focus_scope {
            return action == "key_press" && key?.valid == true && key?.name == scope.key &&
                value == nil && mouse == nil && scroll == nil && original_proof.menu_scope == nil
        }
        guard key == nil else { return false }
        if let scope = original_proof.menu_scope {
            guard let attempt_id, mouse == nil, value == nil, scroll == nil else { return false }
            switch scope {
            case .opener: return action == "ax_show_menu"
            case .item(_, let opening): return action == "ax_menu_press" && opening.attempt_id == attempt_id
            }
        }
        if action == "scroll" {
            return value == nil && mouse == nil && scroll?.valid == true && original_proof.fingerprint.role == kAXScrollAreaRole
        }
        guard scroll == nil else { return false }
        if action == "click" { return value == nil && mouse?.valid == true }
        guard mouse == nil else { return false }
        if action == "pointer_move" {
            return value == nil && HitRequest.roles(action: "pointer_move").contains(original_proof.fingerprint.role)
        }
        if action == "ax_show_menu" {
            return value == nil && [kAXMenuButtonRole, kAXPopUpButtonRole].contains(original_proof.fingerprint.role)
        }
        if action == "activate_window" {
            let fp = original_proof.fingerprint
            return value == nil && fp.role == kAXWindowRole && fp.subrole == kAXStandardWindowSubrole &&
                fp.ancestor_roles.isEmpty && fp.group == nil && fp.sibling_index == 0 && fp.scroll_container == nil
        }
        guard action == "ax_set_value" else { return action == "ax_press" && value == nil }
        guard let value, value.utf8.count <= 2048, !value.unicodeScalars.contains(where: { $0.properties.generalCategory == .control && ![9,10,13].contains($0.value) }) else { return false }
        let fp = original_proof.fingerprint
        if ["AXCheckBox", "AXRadioButton"].contains(fp.role) { return ["on", "off"].contains(value) }
        return ["com.lingxi.computer-user.guard-fixture", "com.lingxi.computer-user.mvp-fixture", "com.apple.TextEdit"].contains(fp.app) &&
            ["AXTextField", "AXTextArea"].contains(fp.role) && (fp.subrole == nil || fp.subrole == "")
    }
    var validMenuProvenance: Bool {
        guard let current = current_proof.menu_scope else { return original_proof.menu_scope == nil }
        guard let original = original_proof.menu_scope else { return false }
        return current.anchor.verified_source_sequence == source_sequence && current.anchor.verified_observed_ms == observed_ms &&
            valid_until_ms <= current.attemptDeadline && command_end_ms <= current.attemptDeadline &&
            current.anchor.verified_source_sequence >= original.anchor.verified_source_sequence &&
            current.anchor.verified_observed_ms >= original.anchor.verified_observed_ms
    }
    var validFocusProvenance: Bool {
        guard let current = current_proof.focus_scope else { return original_proof.focus_scope == nil && action != "key_press" }
        guard let original = original_proof.focus_scope else { return false }
        return current.context.verified_source_sequence == source_sequence && current.context.verified_observed_ms == observed_ms &&
            current.context.verified_source_sequence >= original.context.verified_source_sequence &&
            current.context.verified_observed_ms >= original.context.verified_observed_ms
    }
}

struct NativeProbeResponse: Encodable, Sendable {
    let protocol_version = 1, kind = "authority_probe"
    let session_epoch: UInt64, request_id: String
    var outcome: String, reason: String
    var binding_digest: String? = nil
    let input_authorized = false
}

func unixMS() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1000)) }

/// One conservative wall/uptime anchor. Arrival of a queued request does not
/// create a new relative TTL. Clock divergence closes this authority session.
struct NativeAuthorityClock: Sendable {
    let origin: UInt64, wall: UInt64, uptime: UInt64, surface: NativeSurface, domain: String
    init?(origin: UInt64, surface: NativeSurface, domain: String, epoch: UInt64,
          wall: UInt64, uptime: UInt64) {
        guard origin <= wall, nativeID(surface.id), surface.epoch > 0, domain == "macos/\(epoch)/wall" else { return nil }
        self.origin = origin; self.surface = surface; self.domain = domain; self.wall = wall; self.uptime = uptime
    }
    func healthy(wall: UInt64, uptime: UInt64) -> Bool {
        guard wall >= self.wall, uptime >= self.uptime else { return false }
        let a = wall - self.wall, b = uptime - self.uptime
        return a >= b ? a - b <= 100 : b - a <= 100
    }
    func deadline(_ shared: UInt64) -> UInt64? {
        let baseline = wall - origin
        guard shared > baseline, uptime <= UInt64.max - (shared - baseline) else { return nil }
        return uptime + (shared - baseline) - 1
    }
    func sourceNow(uptime current: UInt64) -> UInt64? {
        guard current >= uptime else { return nil }
        let baseline = wall - origin
        let elapsed = current - uptime
        guard baseline <= UInt64.max - elapsed else { return nil }
        return baseline + elapsed
    }
}

struct NativeProbeTicket: Sendable, Equatable {
    let request: NativeProbe
    let deadline: UInt64
}

struct NativeMenuOpeningAudit: Sendable {
    let opening: NativeProbe
    let returned_ms: UInt64
    var selected = false
}

/// Value-only state inside GuardState's single lock. No AX handles or timers.
struct NativeAuthorityLedger: Sendable {
    var clock: NativeAuthorityClock?
    var failedClock = false
    var stopFloor: UInt64 = 0
    var revoked: Set<String> = []
    var intents: [String: NativeProbeTicket] = [:]
    var permits: [String: NativeProbe] = [:]
    var rowSelectionReads: Set<UInt64> = []
    var committed: Set<String> = []
    var menuOpenings: [String: NativeMenuOpeningAudit] = [:]
    var menuAttempts: Set<String> = []
    var ownedValueIntents: [String: NativeOwnedValueTicket] = [:]
    var ownedValuePermits: [String: NativeOwnedValueRequest] = [:]
    var committedOwnedValues: Set<String> = []
    static let limit = 128

    mutating func configure(_ candidate: NativeAuthorityClock) -> Bool {
        guard !failedClock else { return false }
        if let clock { return clock.origin == candidate.origin && clock.surface == candidate.surface && clock.domain == candidate.domain }
        clock = candidate; return true
    }
    mutating func checkClock(wall: UInt64, uptime: UInt64) -> Bool {
        guard let clock, !failedClock else { return false }
        guard clock.healthy(wall: wall, uptime: uptime) else { failedClock = true; return false }
        return true
    }
    mutating func revoke(_ permit: NativePermit) -> Bool {
        guard nativeID(permit.id), permit.generation > 0, revoked.count < Self.limit || revoked.contains(permit.id) else { return false }
        revoked.insert(permit.id); return true
    }
    mutating func consumeRowSelectionRead(_ sourceSequence: UInt64) -> Bool {
        guard sourceSequence > 0, rowSelectionReads.count < Self.limit,
              !rowSelectionReads.contains(sourceSequence) else { return false }
        rowSelectionReads.insert(sourceSequence)
        return true
    }
    mutating func admit(_ request: NativeProbe, now: UInt64, wall: UInt64) -> (NativeProbeTicket?, String) {
        guard request.valid, checkClock(wall: wall, uptime: now), let clock,
              request.surface == clock.surface, request.clock_domain == clock.domain else { return (nil, "context_unresolved") }
        guard request.stop_seq >= stopFloor else { return (nil, "stop_latched") }
        guard !revoked.contains(request.permit.id) else { return (nil, "revoked") }
        if let invalid = menuValidity(request) { return (nil, invalid) }
        guard let deadline = clock.deadline(request.valid_until_ms), deadline > now, deadline - now <= 1500,
              (clock.deadline(request.observed_ms).map({ $0 <= now + 2 }) ?? (request.observed_ms <= clock.wall - clock.origin)),
              (clock.deadline(request.started_ms).map({ $0 <= now + 2 }) ?? (request.started_ms <= clock.wall - clock.origin)) else { return (nil, "expired") }
        guard intents[request.intent_id] == nil, intents.count < Self.limit,
              permits[request.permit.id] != nil || permits.count < Self.limit else { return (nil, "rejected") }
        let rowReadKey = request.action == "ax_set_selected_true"
            ? request.source_sequence : nil
        if let old = permits[request.permit.id] {
            // Read-scoped authority is one action from one source read. A permit
            // cannot be refreshed into or out of that mode, even on the same id.
            guard old.action != "ax_set_selected_true", request.action != "ax_set_selected_true" else {
                return (nil, "rejected")
            }
            guard old.original_proof.continuity.kind == .notificationCovered,
                  request.original_proof.continuity.kind == .notificationCovered else { return (nil, "rejected") }
            guard old.task_id == request.task_id, old.task_revision == request.task_revision, old.root_epoch == request.root_epoch, old.attempt_id == request.attempt_id,
                  old.permit == request.permit, old.original_proof.sameInstance(request.original_proof),
                  old.dependencies == request.dependencies,
                  request.source_sequence >= old.source_sequence,
                request.current_proof.environment_revision >= old.current_proof.environment_revision else { return (nil, "rejected") }
        }
        if let rowReadKey, !consumeRowSelectionRead(rowReadKey) { return (nil, "rejected") }
        stopFloor = request.stop_seq
        let ticket = NativeProbeTicket(request: request, deadline: deadline)
        intents[request.intent_id] = ticket; permits[request.permit.id] = request
        return (ticket, "eligible")
    }
    mutating func validity(_ ticket: NativeProbeTicket, now: UInt64, wall: UInt64) -> String? {
        guard checkClock(wall: wall, uptime: now) else { return "context_unresolved" }
        guard !revoked.contains(ticket.request.permit.id) else { return "revoked" }
        guard now < ticket.deadline else { return "expired" }
        guard intents[ticket.request.intent_id]?.request == ticket.request else { return "rejected" }
        if let invalid = menuValidity(ticket.request) { return invalid }
        return nil
    }
    mutating func stagedKeySubmission(_ request: NativeProbe, now: UInt64, wall: UInt64) -> (NativeProbeTicket?, String) {
        guard request.valid, request.action == "key_press", let ticket = intents[request.intent_id],
              ticket.request == request, !committed.contains(request.intent_id) else { return (nil, "rejected") }
        if let invalid = validity(ticket, now: now, wall: wall) { return (nil, invalid) }
        return (ticket, "eligible")
    }
    mutating func commit(_ ticket: NativeProbeTicket, now: UInt64, wall: UInt64) -> String? {
        if let invalid = validity(ticket, now: now, wall: wall) { return invalid }
        guard !committed.contains(ticket.request.intent_id) else { return "rejected" }
        if let scope = ticket.request.original_proof.menu_scope {
            switch scope {
            case .opener:
                guard menuAttempts.count < 32, menuOpenings.count < 8 else { return "rejected" }
                menuAttempts.insert(menuAttemptKey(ticket.request))
            case .item(_, let opening):
                guard var audit = menuOpenings[opening.intent_id], !audit.selected else { return "rejected" }
                audit.selected = true; menuOpenings[opening.intent_id] = audit
            }
        }
        committed.insert(ticket.request.intent_id)
        return nil
    }
    mutating func admitOwnedValue(
        _ request: NativeOwnedValueRequest,
        now: UInt64,
        wall: UInt64
    ) -> (NativeOwnedValueTicket?, String) {
        guard request.validIdentity,
              checkClock(wall: wall, uptime: now), let clock,
              request.surface == clock.surface,
              request.clock_domain == clock.domain else { return (nil, "context_unresolved") }
        guard request.stop_seq >= stopFloor else { return (nil, "stop_latched") }
        guard !revoked.contains(request.permit.id) else { return (nil, "revoked") }
        guard let deadline = clock.deadline(request.valid_until_ms), deadline > now,
              deadline - now <= 1_500,
              wall >= clock.origin,
              request.observed_ms <= wall - clock.origin,
              request.started_ms <= wall - clock.origin,
              request.valid_until_ms <= request.command_end_ms else { return (nil, "expired") }
        guard ownedValueIntents[request.intent_id] == nil,
              ownedValueIntents.count < Self.limit,
              ownedValuePermits[request.permit.id] != nil || ownedValuePermits.count < Self.limit else {
            return (nil, "rejected")
        }
        if let old = ownedValuePermits[request.permit.id] {
            guard old.task_id == request.task_id,
                  old.task_revision == request.task_revision,
                  old.root_epoch == request.root_epoch,
                  old.attempt_id == request.attempt_id,
                  old.permit == request.permit,
                  old.dependencies == request.dependencies,
                  old.scopeDigest == request.scopeDigest,
                  request.source_sequence >= old.source_sequence,
                  request.current_proof.environment_revision >= old.current_proof.environment_revision else {
                return (nil, "rejected")
            }
        }
        stopFloor = request.stop_seq
        let ticket = NativeOwnedValueTicket(request: request, deadline: deadline)
        ownedValueIntents[request.intent_id] = ticket
        ownedValuePermits[request.permit.id] = request
        return (ticket, "eligible")
    }
    mutating func ownedValueValidity(
        _ ticket: NativeOwnedValueTicket,
        now: UInt64,
        wall: UInt64
    ) -> String? {
        guard checkClock(wall: wall, uptime: now), let clock else { return "context_unresolved" }
        guard !revoked.contains(ticket.request.permit.id) else { return "revoked" }
        guard now < ticket.deadline,
              wall >= clock.origin,
              wall - clock.origin < ticket.request.valid_until_ms else { return "expired" }
        guard ownedValueIntents[ticket.request.intent_id] == ticket,
              !committedOwnedValues.contains(ticket.request.intent_id) else { return "rejected" }
        return nil
    }
    mutating func commitOwnedValue(
        _ ticket: NativeOwnedValueTicket,
        now: UInt64,
        wall: UInt64
    ) -> String? {
        if let invalid = ownedValueValidity(ticket, now: now, wall: wall) { return invalid }
        committedOwnedValues.insert(ticket.request.intent_id)
        return nil
    }

    func menuAttemptKey(_ request: NativeProbe) -> String {
        "\(request.task_id)\u{1f}\(request.task_revision)\u{1f}\(request.root_epoch)\u{1f}\(request.attempt_id ?? "")"
    }
    func menuValidity(_ request: NativeProbe) -> String? {
        guard let scope = request.current_proof.menu_scope else {
            return menuAttempts.contains(menuAttemptKey(request)) ? "rejected" : nil
        }
        switch scope {
        case .opener:
            return menuAttempts.contains(menuAttemptKey(request)) ? "rejected" : nil
        case .item(let context, let reference):
            guard let audit = menuOpenings[reference.intent_id], !audit.selected,
                  audit.opening.attempt_id == reference.attempt_id, request.attempt_id == reference.attempt_id,
                  request.task_id == audit.opening.task_id, request.task_revision == audit.opening.task_revision,
                  request.root_epoch == audit.opening.root_epoch, request.guard_revision == audit.opening.guard_revision,
                  case .opener(let anchor, let ceiling) = audit.opening.current_proof.menu_scope,
                  anchor.sameIdentity(context.opener), request.source_sequence > audit.opening.source_sequence,
                  request.observed_ms > audit.returned_ms,
                  reference.attempt_deadline_ms == ceiling,
                  request.valid_until_ms <= ceiling, request.command_end_ms <= ceiling else { return "context_unresolved" }
            return nil
        }
    }
    mutating func recordMenuOpening(_ ticket: NativeProbeTicket, response: NativeSubmitResponse) -> Bool {
        guard case .opener = ticket.request.current_proof.menu_scope else { return true }
        guard response.submitted, response.outcome == "applied", let returned = response.returned_ms,
              committed.contains(ticket.request.intent_id), menuOpenings[ticket.request.intent_id] == nil,
              menuOpenings.count < 8, returned < ticket.request.current_proof.menu_scope!.attemptDeadline else { return false }
        menuOpenings[ticket.request.intent_id] = NativeMenuOpeningAudit(opening: ticket.request, returned_ms: returned)
        return true
    }
}

func strictProbeFields(_ raw: [String: Any]) -> Bool {
    let fields = Set(raw.keys)
    guard let encoded = try? JSONSerialization.data(withJSONObject: raw), encoded.count <= 8192,
          fields.isSuperset(of: NativeProbe.keys), fields.isSubset(of: NativeProbe.keys.union([
              "value", "mouse", "scroll", "attempt_id", "key", "execution_domain", "purpose", "coordinate_space"])),
          let permit = raw["permit"] as? [String: Any], Set(permit.keys) == ["id", "generation"],
          let surface = raw["surface"] as? [String: Any], Set(surface.keys) == ["id", "epoch"],
          strictNativeDeviceContextWitnessFields(raw["context_witness"]) else { return false }
    if let domain = raw["execution_domain"], !(domain is String) { return false }
    if let purpose = raw["purpose"], !(purpose is String) { return false }
    if let space = raw["coordinate_space"], !(space is String) { return false }
    for key in ["original_proof", "current_proof"] {
        guard let proof = raw[key] as? [String: Any], Set(proof.keys).isSuperset(of: NativeSourceProof.keys), Set(proof.keys).isSubset(of: NativeSourceProof.keys.union(["menu_scope", "focus_scope"])),
              let fp = proof["fingerprint"] as? [String: Any], Set(fp.keys) == NativeBindingFingerprint.keys,
              let continuity = proof["continuity"] as? [String: Any], strictBindingContinuityFields(continuity),
              let frame = proof["display_frame"] as? [String: Any], Set(frame.keys) == ["x", "y", "width", "height"] else { return false }
        if let scope = proof["menu_scope"], !strictMenuScopeFields(scope) { return false }
        if let scope = proof["focus_scope"], !strictFocusScopeFields(scope) { return false }
    }
    if let scroll = raw["scroll"] {
        guard let scroll = scroll as? [String: Any], Set(scroll.keys) == ["dx", "dy"] else { return false }
    }
    if let mouse = raw["mouse"] {
        guard let mouse = mouse as? [String: Any], Set(mouse.keys) == ["button", "modifiers"],
              let modifiers = mouse["modifiers"] as? [String: Any], Set(modifiers.keys) == ["shift", "control", "option", "command"] else { return false }
    }
    if let key = raw["key"] {
        guard let key = key as? [String: Any], Set(key.keys) == ["name", "modifiers"],
              let modifiers = key["modifiers"] as? [String: Any], Set(modifiers.keys) == ["shift", "control", "option", "command"] else { return false }
    }
    return true
}

func strictBindingContinuityFields(_ raw: [String: Any]) -> Bool {
    guard Set(raw.keys) == NativeBindingContinuityProof.keys,
          let kind = raw["kind"] as? String,
          let sequence = raw["source_sequence"] as? UInt64, sequence > 0,
          let digest = raw["context_digest"] as? String, nativeBindingContextDigest(digest),
          let scope = raw["scope"] as? [String: Any], let scopeKind = scope["kind"] as? String else { return false }
    switch scopeKind {
    case "whole_structure":
        return Set(scope.keys) == ["kind"] && ["notification_covered", "read_scoped"].contains(kind)
    case "table_viewport":
        guard Set(scope.keys) == ["kind", "table_instance_ref", "scroll_container_instance_ref", "viewport_frame", "visible_rows_complete", "columns_complete"],
              let table = scope["table_instance_ref"] as? String, nativeID(table),
              let container = scope["scroll_container_instance_ref"] as? String, nativeID(container),
              let frame = scope["viewport_frame"] as? [String: Any],
              Set(frame.keys) == ["x", "y", "width", "height"],
              let x = frame["x"] as? Double, let y = frame["y"] as? Double,
              let width = frame["width"] as? Double, let height = frame["height"] as? Double,
              [x, y, width, height].allSatisfy({ $0.isFinite && abs($0) <= 100_000 }),
              width > 0, height > 0,
              scope["visible_rows_complete"] is Bool, scope["columns_complete"] is Bool else { return false }
        return ["notification_covered", "read_scoped"].contains(kind)
    case "window_root":
        guard Set(scope.keys) == ["kind", "table_scope"],
              strictNativeRowTableScopeFields(scope["table_scope"]) else { return false }
        return ["notification_covered", "read_scoped"].contains(kind)
    case "background_ax":
        guard Set(scope.keys) == ["kind", "target_context_hash"],
              let targetContextHash = scope["target_context_hash"] as? String,
              nativeTargetContextHash(targetContextHash) else { return false }
        return ["notification_covered", "read_scoped"].contains(kind)
    default:
        return false
    }
}
