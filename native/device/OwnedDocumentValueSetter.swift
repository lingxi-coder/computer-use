import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import Foundation

/// Main's private, startup-only document identity. This descriptor is not an
/// execution grant; the Device guard must separately authorize a setter.
struct NativeOwnedDocumentRegistration: Decodable, Sendable, Equatable {
    let version: UInt16
    let registration_id: String
    let pid: UInt32
    let start_seconds: UInt64
    let start_micros: UInt64
    let executable_path: String
    let bundle_id: String
    let document_path: String
    let document_sha256: String
    let document_dev: UInt64
    let document_ino: UInt64
    let directory_dev: UInt64
    let directory_ino: UInt64
    let issued_unix_ms: UInt64
    private let startupIdentityVerified: Bool

    private enum CodingKeys: String, CodingKey {
        case version, registration_id, pid, start_seconds, start_micros
        case executable_path, bundle_id, document_path, document_sha256
        case document_dev, document_ino, directory_dev, directory_ino, issued_unix_ms
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        version = try values.decode(UInt16.self, forKey: .version)
        registration_id = try values.decode(String.self, forKey: .registration_id)
        pid = try values.decode(UInt32.self, forKey: .pid)
        start_seconds = try values.decode(UInt64.self, forKey: .start_seconds)
        start_micros = try values.decode(UInt64.self, forKey: .start_micros)
        executable_path = try values.decode(String.self, forKey: .executable_path)
        bundle_id = try values.decode(String.self, forKey: .bundle_id)
        document_path = try values.decode(String.self, forKey: .document_path)
        document_sha256 = try values.decode(String.self, forKey: .document_sha256)
        document_dev = try values.decode(UInt64.self, forKey: .document_dev)
        document_ino = try values.decode(UInt64.self, forKey: .document_ino)
        directory_dev = try values.decode(UInt64.self, forKey: .directory_dev)
        directory_ino = try values.decode(UInt64.self, forKey: .directory_ino)
        issued_unix_ms = try values.decode(UInt64.self, forKey: .issued_unix_ms)
        startupIdentityVerified = false
    }

    private init(copying value: NativeOwnedDocumentRegistration, startupIdentityVerified: Bool) {
        version = value.version
        registration_id = value.registration_id
        pid = value.pid
        start_seconds = value.start_seconds
        start_micros = value.start_micros
        executable_path = value.executable_path
        bundle_id = value.bundle_id
        document_path = value.document_path
        document_sha256 = value.document_sha256
        document_dev = value.document_dev
        document_ino = value.document_ino
        directory_dev = value.directory_dev
        directory_ino = value.directory_ino
        issued_unix_ms = value.issued_unix_ms
        self.startupIdentityVerified = startupIdentityVerified
    }

    static let maximumBytes = 8192
    static let wireKeys: Set<String> = [
        "version", "registration_id", "pid", "start_seconds", "start_micros",
        "executable_path", "bundle_id", "document_path", "document_sha256",
        "document_dev", "document_ino", "directory_dev", "directory_ino", "issued_unix_ms",
    ]

    /// Strictly decodes the exact private registration and checks its filesystem
    /// and process identity. A verified registration is still not setter authority.
    static func decode(_ data: Data, nowUnixMS: UInt64) throws -> Self {
        let value = try decodeForPureValidation(data, nowUnixMS: nowUnixMS)
        guard value.verifyLiveIdentity(checkDocumentHash: true) else {
            throw OwnedDocumentValueSetterError.invalidRegistration
        }
        return Self(copying: value, startupIdentityVerified: true)
    }

    /// Test-only value projection. The returned value is explicitly unusable
    /// by `withVerifiedOwnedReceiver` because no startup identity was verified.
    static func decodeForPureValidation(_ data: Data, nowUnixMS: UInt64) throws -> Self {
        guard !data.isEmpty, data.count <= maximumBytes,
              ownedJSONHasNoDuplicateKeys(data),
              let raw = try? JSONSerialization.jsonObject(with: data),
              ownedExactDictionary(raw, keys: wireKeys) != nil,
              let value = try? JSONDecoder().decode(Self.self, from: data),
              value.validStructure(), value.validUnixIssuance(nowUnixMS: nowUnixMS) else {
            throw OwnedDocumentValueSetterError.invalidRegistration
        }
        return value
    }

    static func decodeAndVerify(_ data: Data, nowUnixMS: UInt64) throws -> Self {
        try decode(data, nowUnixMS: nowUnixMS)
    }

    fileprivate func validStructure() -> Bool {
        let pathParts = document_path.split(separator: "/", omittingEmptySubsequences: false)
        let normalizedAbsolutePath = document_path.hasPrefix("/")
            && document_path.utf8.count <= 4096
            && pathParts.count > 1
            && pathParts.dropFirst().allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
            && pathParts.last == "document.txt"
            && !document_path.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
        let canonicalRegistrationID = UUID(uuidString: registration_id)?.uuidString.lowercased() == registration_id
        let digestValid = ownedLowerHex(document_sha256, length: 64)
        return version == 1 && canonicalRegistrationID
            && pid > 0 && pid <= UInt32(Int32.max)
            && start_seconds > 0 && start_micros < 1_000_000
            && executable_path == Self.textEditExecutablePath
            && bundle_id == Self.textEditBundleID
            && normalizedAbsolutePath && digestValid
            && document_ino > 0 && directory_ino > 0
            && [document_dev, document_ino, directory_dev, directory_ino]
                .allSatisfy { $0 <= 9_007_199_254_740_991 }
            && issued_unix_ms > 0
    }

    fileprivate func validUnixIssuance(nowUnixMS: UInt64) -> Bool {
        issued_unix_ms <= nowUnixMS
            ? nowUnixMS - issued_unix_ms <= 5_000
            : issued_unix_ms - nowUnixMS <= 1_000
    }

    /// Rechecks process/path/inode/mode identity on each native probe. The
    /// initial file digest is checked by `decodeAndVerify`; later saves may
    /// change contents while retaining file identity. This only reuses the same
    /// startup identity in an already-authorized scope; it is not a renewal.
    func verifyLiveIdentity(checkDocumentHash: Bool) -> Bool {
        let pid = Int32(self.pid)
        let expectedStamp = DeviceProcessStamp(seconds: start_seconds, micros: start_micros)
        guard DeviceProcessStamp.read(pid) == expectedStamp,
              let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.bundleIdentifier == bundle_id,
              ownedExecutablePath(self.pid) == executable_path,
              ownedCanonicalPath(document_path) == document_path,
              let temporaryRoot = ownedCanonicalPath(NSTemporaryDirectory()),
              document_path.hasPrefix(temporaryRoot + "/") else { return false }

        let directory = (document_path as NSString).deletingLastPathComponent
        var directoryStat = stat()
        var fileStat = stat()
        guard lstat(directory, &directoryStat) == 0,
              lstat(document_path, &fileStat) == 0,
              (directoryStat.st_mode & S_IFMT) == S_IFDIR,
              (fileStat.st_mode & S_IFMT) == S_IFREG,
              directoryStat.st_uid == getuid(), fileStat.st_uid == getuid(),
              (directoryStat.st_mode & 0o777) == 0o700,
              (fileStat.st_mode & 0o777) == 0o600,
              directoryStat.st_dev >= 0, fileStat.st_dev >= 0,
              UInt64(directoryStat.st_dev) == directory_dev,
              UInt64(directoryStat.st_ino) == directory_ino,
              UInt64(fileStat.st_dev) == document_dev,
              UInt64(fileStat.st_ino) == document_ino,
              fileStat.st_nlink == 1,
              fileStat.st_size >= 0, fileStat.st_size <= 4096 else { return false }

        let descriptor = open(document_path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { return false }
        defer { _ = close(descriptor) }
        var opened = stat()
        guard fstat(descriptor, &opened) == 0,
              opened.st_dev == fileStat.st_dev, opened.st_ino == fileStat.st_ino,
              opened.st_size == fileStat.st_size else { return false }
        if checkDocumentHash {
            guard let bytes = ownedReadBoundedFile(descriptor, expectedSize: fileStat.st_size),
                  SHA256.hash(data: bytes).map({ String(format: "%02x", $0) }).joined() == document_sha256 else {
                return false
            }
        }
        var finalFile = stat()
        var finalDirectory = stat()
        return fstat(descriptor, &finalFile) == 0
            && finalFile.st_dev == fileStat.st_dev && finalFile.st_ino == fileStat.st_ino
            && finalFile.st_size == fileStat.st_size && finalFile.st_nlink == 1
            && lstat(document_path, &finalFile) == 0
            && finalFile.st_dev == fileStat.st_dev && finalFile.st_ino == fileStat.st_ino
            && (finalFile.st_mode & S_IFMT) == S_IFREG && finalFile.st_uid == getuid()
            && (finalFile.st_mode & 0o777) == 0o600 && finalFile.st_nlink == 1
            && lstat(directory, &finalDirectory) == 0
            && (finalDirectory.st_mode & S_IFMT) == S_IFDIR && finalDirectory.st_uid == getuid()
            && (finalDirectory.st_mode & 0o777) == 0o700
            && finalDirectory.st_dev == directoryStat.st_dev
            && finalDirectory.st_ino == directoryStat.st_ino
            && DeviceProcessStamp.read(pid) == expectedStamp
    }

    fileprivate var isStartupIdentityVerified: Bool { startupIdentityVerified }

    static let textEditBundleID = "com.apple.TextEdit"
    static let textEditExecutablePath = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
}

/// The typed, seven-field contract wrapper. Source object refs remain opaque
/// proof values; they are never looked up as native AX references.
struct NativeOwnedDocumentBindingProof: Decodable, Sendable, Equatable {
    let binding: String
    let semantic_target: String
    let environment_revision: UInt64
    let session_epoch: UInt64
    let surface: NativeSurface
    let snapshot_observed_ms: UInt64
    fileprivate let receiver: NativeOwnedDocumentReceiverEvidence

    static let wireKeys: Set<String> = [
        "binding", "semantic_target", "environment_revision", "session_epoch",
        "surface", "snapshot_observed_ms", "receiver",
    ]

    static func decode(
        _ data: Data,
        registration: NativeOwnedDocumentRegistration,
        expectedSessionEpoch: UInt64,
        expectedSurface: NativeSurface,
        expectedClockDomain: String,
        nowSourceMS: UInt64,
        maxAgeMS: UInt64
    ) throws -> Self {
        guard !data.isEmpty, data.count <= 64 * 1024,
              ownedJSONHasNoDuplicateKeys(data),
              let raw = try? JSONSerialization.jsonObject(with: data),
              strictOwnedDocumentProofShape(raw),
              let value = try? JSONDecoder().decode(Self.self, from: data),
              value.valid(
                registration: registration,
                expectedSessionEpoch: expectedSessionEpoch,
                expectedSurface: expectedSurface,
                expectedClockDomain: expectedClockDomain,
                nowSourceMS: nowSourceMS,
                maxAgeMS: maxAgeMS
              ) else {
            throw OwnedDocumentValueSetterError.invalidProof
        }
        return value
    }

    fileprivate func valid(
        registration: NativeOwnedDocumentRegistration,
        expectedSessionEpoch: UInt64,
        expectedSurface: NativeSurface,
        expectedClockDomain: String,
        nowSourceMS: UInt64,
        maxAgeMS: UInt64
    ) -> Bool {
        guard nativeID(binding), nativeID(semantic_target),
              environment_revision > 0,
              session_epoch == expectedSessionEpoch, session_epoch > 0,
              surface == expectedSurface, surface.epoch > 0,
              snapshot_observed_ms > 0, maxAgeMS > 0,
              nativeID(expectedClockDomain), expectedClockDomain.utf8.count <= 128,
              receiver.registration_id == registration.registration_id,
              receiver.process.pid == registration.pid,
              receiver.process.start_seconds == registration.start_seconds,
              receiver.process.start_micros == registration.start_micros,
              receiver.provenance.clock_domain == expectedClockDomain,
              receiver.validSourceFacts(snapshotObservedMS: snapshot_observed_ms,
                  nowSourceMS: nowSourceMS, maxAgeMS: maxAgeMS) else { return false }
        return true
    }

    fileprivate func isFresh(nowSourceMS: UInt64, maxAgeMS: UInt64) -> Bool {
        nowSourceMS >= snapshot_observed_ms
            && nowSourceMS < receiver.provenance.valid_until_ms
            && nowSourceMS >= receiver.provenance.observed_ms
            && nowSourceMS.saturatingSubtract(snapshot_observed_ms) < maxAgeMS
            && nowSourceMS.saturatingSubtract(receiver.provenance.observed_ms) < maxAgeMS
    }
}

private struct NativeOwnedProcessIdentity: Decodable, Sendable, Equatable {
    let pid: UInt32
    let start_seconds: UInt64
    let start_micros: UInt64
}

private struct NativeOwnedIdentityBoundary: Decodable, Sendable, Equatable {
    let namespace: String
    let generation: UInt64
}

private struct NativeOwnedProvenance: Decodable, Sendable, Equatable {
    let identity_boundary: NativeOwnedIdentityBoundary
    let snapshot_id: String
    let source_sequence: UInt64
    let observed_ms: UInt64
    let valid_until_ms: UInt64
    let clock_domain: String
}

private struct NativeOwnedRect: Decodable, Sendable, Equatable {
    let x: Double
    let y: Double
    let width: Double
    let height: Double

    var valid: Bool {
        [x, y, width, height].allSatisfy { $0.isFinite && abs($0) <= 100_000 }
            && width > 0 && height > 0
    }

    func contains(_ other: NativeOwnedRect) -> Bool {
        valid && other.valid && other.x >= x && other.y >= y
            && other.x + other.width <= x + width
            && other.y + other.height <= y + height
    }

    func matches(_ other: HitRect) -> Bool {
        zip([x, y, width, height], [other.x, other.y, other.width, other.height])
            .allSatisfy { abs($0 - $1) < 0.5 }
    }
}

private struct NativeOwnedAncestor: Decodable, Sendable, Equatable {
    let object_ref: String
    let role: String
}

private struct NativeOwnedDocumentRead: Decodable, Sendable, Equatable {
    let status: String
    let api_code: Int32?
    let cf_type_id: UInt64?
    let observed_ms: UInt64
    let timely: Bool
    let registered_path_matches: Bool

    var valueOK: Bool {
        status == "value" && api_code == 0 && cf_type_id.map { $0 > 0 } == true
            && timely && registered_path_matches && observed_ms > 0
    }
}

private struct NativeOwnedDocumentBinding: Decodable, Sendable, Equatable {
    let before: NativeOwnedDocumentRead
    let after: NativeOwnedDocumentRead
}

private struct NativeOwnedTextRange: Decodable, Sendable, Equatable {
    let location: Int64
    let length: Int64

    var unmarked: Bool { location >= 0 && length == 0 }
}

private struct NativeOwnedMarkedRange: Decodable, Sendable, Equatable {
    let receiver_object_ref: String
    let receiver_same_cf_as_actual_object_ref: Bool
    let observed_ms: UInt64
    let observation: NativeOwnedAXObservation<NativeOwnedTextRange>

    var range: NativeOwnedTextRange? {
        guard receiver_same_cf_as_actual_object_ref, observed_ms > 0,
              case .value(let range) = observation, range.unmarked else { return nil }
        return range
    }
}

private enum NativeOwnedAXObservation<Value: Decodable & Sendable & Equatable>: Decodable, Sendable, Equatable {
    case value(Value)
    case unsupported(Int32)
    case noValue(Int32)
    case failure(Int32)
    case decodeFailure(Int32, UInt64?)
    case notAttempted(String)

    private enum Keys: String, CodingKey { case status, value, code, api_code, actual_cf_type_id, reason }

    init(from decoder: Decoder) throws {
        let fields = try decoder.container(keyedBy: Keys.self)
        switch try fields.decode(String.self, forKey: .status) {
        case "value": self = .value(try fields.decode(Value.self, forKey: .value))
        case "unsupported": self = .unsupported(try fields.decode(Int32.self, forKey: .code))
        case "no_value": self = .noValue(try fields.decode(Int32.self, forKey: .code))
        case "failure": self = .failure(try fields.decode(Int32.self, forKey: .code))
        case "decode_failure":
            self = .decodeFailure(try fields.decode(Int32.self, forKey: .api_code),
                try fields.decodeIfPresent(UInt64.self, forKey: .actual_cf_type_id))
        case "not_attempted": self = .notAttempted(try fields.decode(String.self, forKey: .reason))
        default: throw OwnedDocumentValueSetterError.invalidProof
        }
    }
}

private struct NativeOwnedValueReadback: Decodable, Sendable, Equatable {
    let receiver_object_ref: String
    let receiver_same_cf_as_actual_object_ref: Bool
    let observed_ms: UInt64
    let sha256: String
    let utf8_byte_count: UInt32
}

private struct NativeOwnedDocumentReceiverEvidence: Decodable, Sendable, Equatable {
    static let wireKeys: Set<String> = [
        "registration_id", "profile", "provenance", "process", "window_id", "window_object_ref",
        "window_frame", "actual_object_ref", "receiver_frame", "application_focused_object_ref",
        "system_focused_object_ref", "ancestry", "scope_complete", "profile_eligible",
        "marked_range_stable_unmarked", "observer_state", "secure", "document_binding_verified",
        "document_binding", "title", "role", "subrole", "enabled", "focused", "value_settable",
        "description", "marked_range_before", "marked_range_after", "value_readback",
    ]
    let registration_id: String
    let profile: String
    let provenance: NativeOwnedProvenance
    let process: NativeOwnedProcessIdentity
    let window_id: String
    let window_object_ref: String
    let window_frame: NativeOwnedRect
    let actual_object_ref: String
    let receiver_frame: NativeOwnedRect
    let application_focused_object_ref: String
    let system_focused_object_ref: String
    let ancestry: [NativeOwnedAncestor]
    let scope_complete: Bool
    let profile_eligible: Bool
    let marked_range_stable_unmarked: Bool
    let observer_state: String
    let secure: Bool
    let document_binding_verified: Bool
    let document_binding: NativeOwnedDocumentBinding
    let role: NativeOwnedAXObservation<String>
    let subrole: NativeOwnedAXObservation<String>
    let enabled: NativeOwnedAXObservation<Bool>
    let focused: NativeOwnedAXObservation<Bool>
    let value_settable: NativeOwnedAXObservation<Bool>
    let marked_range_before: NativeOwnedMarkedRange
    let marked_range_after: NativeOwnedMarkedRange
    let value_readback: NativeOwnedValueReadback?

    func validSourceFacts(snapshotObservedMS: UInt64, nowSourceMS: UInt64,
                          maxAgeMS: UInt64) -> Bool {
        let beforeRange = marked_range_before.range
        let afterRange = marked_range_after.range
        let boundary = provenance.identity_boundary
        let ancestryRefs = ancestry.map(\.object_ref)
        let ancestryRolesValid = ancestry.allSatisfy { nativeID($0.object_ref)
            && nativeID($0.role) && $0.role.utf8.count <= 128 && $0.role != "AXSecureTextField" }
        let roleEligible: Bool
        if case .value(let value) = role { roleEligible = value == "AXTextArea" }
        else { roleEligible = false }
        let subroleEligible: Bool
        switch subrole {
        case .value(let value): subroleEligible = value.isEmpty
        case .unsupported(let code): subroleEligible = code == AXError.attributeUnsupported.rawValue
        default: subroleEligible = false
        }
        let enabledEligible: Bool
        switch enabled {
        case .value(let value): enabledEligible = value
        case .unsupported(let code): enabledEligible = code == AXError.attributeUnsupported.rawValue
        default: enabledEligible = false
        }
        let focusedEligible: Bool
        if case .value(let value) = focused { focusedEligible = value }
        else { focusedEligible = false }
        let settableEligible: Bool
        if case .value(let value) = value_settable { settableEligible = value }
        else { settableEligible = false }
        let observerContinuous = observer_state == "continuous"
        let provenanceOK = nativeID(provenance.snapshot_id)
            && provenance.source_sequence > 0 && provenance.observed_ms > 0
            && provenance.valid_until_ms > provenance.observed_ms
            && provenance.clock_domain.utf8.count <= 128
            && nativeID(provenance.clock_domain)
            && UUID(uuidString: boundary.namespace)?.uuidString.lowercased() == boundary.namespace
            && boundary.generation > 0
            && snapshotObservedMS >= provenance.observed_ms
            && nowSourceMS >= snapshotObservedMS
            && nowSourceMS < provenance.valid_until_ms
            && nowSourceMS.saturatingSubtract(snapshotObservedMS) < maxAgeMS
            && nowSourceMS.saturatingSubtract(provenance.observed_ms) < maxAgeMS
        let docBindingOK = document_binding.before.valueOK && document_binding.after.valueOK
            && document_binding.after.observed_ms >= document_binding.before.observed_ms
            && document_binding.before.observed_ms >= provenance.observed_ms
            && document_binding.after.observed_ms < provenance.valid_until_ms
        let readbackOK: Bool
        if let readback = value_readback {
            readbackOK = readback.receiver_object_ref == actual_object_ref
                && readback.receiver_same_cf_as_actual_object_ref
                && readback.observed_ms >= provenance.observed_ms
                && readback.observed_ms < provenance.valid_until_ms
                && readback.observed_ms <= nowSourceMS
                && readback.utf8_byte_count <= 2048
                && ownedLowerHex(readback.sha256, length: 64)
        } else { readbackOK = true }
        return nativeID(registration_id) && registration_id.utf8.count <= 256
            && profile == "owned_plain_text_value_setter_v1"
            && process.pid > 0 && process.start_seconds > 0 && process.start_micros < 1_000_000
            && nativeID(window_id) && window_id.hasPrefix("window-")
            && UInt32(window_id.dropFirst("window-".count)).map { $0 > 0 } == true
            && nativeID(window_object_ref) && nativeID(actual_object_ref)
            && nativeID(application_focused_object_ref) && nativeID(system_focused_object_ref)
            && actual_object_ref != window_object_ref
            && application_focused_object_ref == actual_object_ref
            && system_focused_object_ref == actual_object_ref
            && window_frame.valid && receiver_frame.valid && window_frame.contains(receiver_frame)
            && ancestry.count >= 1 && ancestry.count <= 8 && ancestryRolesValid
            && Set(ancestryRefs).count == ancestryRefs.count
            && !ancestryRefs.contains(actual_object_ref)
            && ancestry.last?.object_ref == window_object_ref
            && ancestry.last?.role == kAXWindowRole
            && scope_complete && profile_eligible && marked_range_stable_unmarked
            && observerContinuous && provenanceOK && !secure
            && document_binding_verified && docBindingOK
            && roleEligible && subroleEligible && enabledEligible && focusedEligible && settableEligible
            && beforeRange != nil && beforeRange == afterRange
            && marked_range_before.receiver_object_ref == actual_object_ref
            && marked_range_after.receiver_object_ref == actual_object_ref
            && marked_range_before.observed_ms >= provenance.observed_ms
            && marked_range_after.observed_ms >= marked_range_before.observed_ms
            && marked_range_after.observed_ms < provenance.valid_until_ms
            && readbackOK
    }
}

private enum OwnedDocumentValueSetterError: Error {
    case invalidRegistration
    case invalidProof
}

/// The setter's content bound uses UTF-8 bytes and does not normalize text.
/// It is a value-policy check only; it never grants execution.
func ownedDocumentSetterValueIsValid(_ value: String) -> Bool {
    value.utf8.count <= 2048
        && !value.unicodeScalars.contains {
            $0.properties.generalCategory == .control && ![9, 10, 13].contains($0.value)
        }
}

/// Performs an independent, label-free native re-probe and lends the receiver
/// only to this worker-stack closure. This helper has no input calls and makes
/// no permission decision; the caller must hold separate Main-issued authority
/// and must enter the ordered guard commit gate before invoking a setter.
enum OwnedReceiverProbeStage: String, Sendable {
    case initial, process, window_inventory, focus, window, tree, receiver, frame, marked, revalidation, final, verified
    case focus_timeout, focus_application, focus_application_match, focus_window
    case focus_system_receiver, focus_app_receiver, focus_receiver_match, focus_windows
    case focus_application_late, focus_application_error, focus_application_type
    case focus_application_cannot_complete, focus_application_no_value, focus_application_api_disabled
    case window_membership, window_attributes, window_document, window_sheets, window_frame, window_match
}

enum OwnedSheetInventoryDisposition: Equatable {
    case empty, unsupportedRequiresTree
}

func ownedSheetInventoryDisposition(error: Int32, typeID: UInt64?, rawCount: Int?,
    convertedCount: Int?, timely: Bool) -> OwnedSheetInventoryDisposition? {
    guard timely else { return nil }
    if error == AXError.attributeUnsupported.rawValue {
        return typeID == nil && rawCount == nil && convertedCount == nil ? .unsupportedRequiresTree : nil
    }
    if error == AXError.noValue.rawValue {
        return typeID == nil && rawCount == nil && convertedCount == nil ? .empty : nil
    }
    return error == AXError.success.rawValue && typeID == UInt64(CFArrayGetTypeID())
        && rawCount == 0 && convertedCount == 0 ? .empty : nil
}

func ownedNodeIsNonTransient(role: String, modalError: Int32, modalTypeID: UInt64?,
    modalValue: Bool?, timely: Bool) -> Bool {
    guard timely, ![kAXSheetRole, "AXDialog", "AXPopover", kAXMenuRole].contains(role) else { return false }
    if modalError == AXError.attributeUnsupported.rawValue || modalError == AXError.noValue.rawValue {
        return role != kAXWindowRole && modalTypeID == nil && modalValue == nil
    }
    return modalError == AXError.success.rawValue && modalTypeID == UInt64(CFBooleanGetTypeID())
        && modalValue == false
}

enum OwnedDocumentValuePolicy {
    case setter
    case save
}

func withVerifiedOwnedReceiver<T>(
    registration: NativeOwnedDocumentRegistration,
    proof: NativeOwnedDocumentBindingProof,
    setterValue: String,
    valuePolicy: OwnedDocumentValuePolicy = .setter,
    expectedSessionEpoch: UInt64,
    expectedSurface: NativeSurface,
    expectedClockDomain: String,
    maxAgeMS: UInt64,
    sourceNowMS: @escaping () -> UInt64,
    deadlineMS: UInt64,
    onStage: @escaping (OwnedReceiverProbeStage) -> Void = { _ in },
    onVerified: (AXUIElement) -> T
) -> T? {
    onStage(.initial)
    guard (valuePolicy == .save
               ? ownedDocumentSaveValueIsValid(setterValue)
               : ownedDocumentSetterValueIsValid(setterValue)),
          deadlineMS > monotonicMS(), maxAgeMS > 0,
          registration.isStartupIdentityVerified,
          registration.validStructure(),
          registration.verifyLiveIdentity(checkDocumentHash: false),
          proof.valid(registration: registration,
              expectedSessionEpoch: expectedSessionEpoch,
              expectedSurface: expectedSurface,
              expectedClockDomain: expectedClockDomain,
              nowSourceMS: sourceNowMS(), maxAgeMS: maxAgeMS),
          AXIsProcessTrusted() else { return nil }
    let result = OwnedDocumentNativeProbe(
        registration: registration,
        proof: proof,
        maxAgeMS: maxAgeMS,
        deadlineMS: deadlineMS,
        sourceNowMS: sourceNowMS,
        onStage: onStage
    ).verify()
    guard let result else { return nil }
    onStage(.final)
    guard deadlineMS > monotonicMS(),
          registration.verifyLiveIdentity(checkDocumentHash: false),
          proof.isFresh(nowSourceMS: sourceNowMS(), maxAgeMS: maxAgeMS),
          proof.valid(registration: registration,
              expectedSessionEpoch: expectedSessionEpoch,
              expectedSurface: expectedSurface,
              expectedClockDomain: expectedClockDomain,
              nowSourceMS: sourceNowMS(), maxAgeMS: maxAgeMS),
          DeviceProcessStamp.read(Int32(registration.pid))
            == DeviceProcessStamp(seconds: registration.start_seconds, micros: registration.start_micros),
          AXIsProcessTrusted() else { return nil }
    onStage(.verified)
    return onVerified(result.receiver)
}

private struct OwnedDocumentNativeProbe {
    let registration: NativeOwnedDocumentRegistration
    let proof: NativeOwnedDocumentBindingProof
    let maxAgeMS: UInt64
    let deadlineMS: UInt64
    let sourceNowMS: () -> UInt64
    let onStage: (OwnedReceiverProbeStage) -> Void

    private var pid: Int32 { Int32(registration.pid) }
    private var expectedStamp: DeviceProcessStamp {
        DeviceProcessStamp(seconds: registration.start_seconds, micros: registration.start_micros)
    }

    func verify() -> (receiver: AXUIElement, window: AXUIElement)? {
        onStage(.process)
        guard beforeDeadline(), liveProcess(),
              let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.bundleIdentifier == registration.bundle_id else { return nil }
        // Establish the WindowServer connection and independently confirm the
        // registered on-screen window before querying system-wide AX focus.
        // The window and its CG identity are checked again below and at return.
        onStage(.window_inventory)
        let expectedFrame = proof.receiver.window_frame
        guard let initialWindowID = uniqueCGWindow(frame: HitRect(CGRect(
                  x: expectedFrame.x, y: expectedFrame.y,
                  width: expectedFrame.width, height: expectedFrame.height)), excludingDesktop: true),
              proof.receiver.window_id == "window-\(initialWindowID)" else { return nil }
        let application = AXUIElementCreateApplication(pid)
        let system = AXUIElementCreateSystemWide()
        onStage(.focus_timeout)
        guard setTimeout(application), setTimeout(system) else { return nil }
        onStage(.focus_application)
        let applicationRead = readRaw(system, kAXFocusedApplicationAttribute)
        guard applicationRead.timely else { onStage(.focus_application_late); return nil }
        guard applicationRead.error == .success else {
            switch applicationRead.error {
            case .cannotComplete: onStage(.focus_application_cannot_complete)
            case .noValue: onStage(.focus_application_no_value)
            case .apiDisabled: onStage(.focus_application_api_disabled)
            default: onStage(.focus_application_error)
            }
            return nil
        }
        guard let applicationValue = applicationRead.value,
              CFGetTypeID(applicationValue) == AXUIElementGetTypeID() else {
            onStage(.focus_application_type); return nil
        }
        let focusedApplicationBefore = unsafeDowncast(applicationValue, to: AXUIElement.self)
        onStage(.focus_application_match)
        guard CFEqual(focusedApplicationBefore, application) else { return nil }
        onStage(.focus_window)
        guard let focusedWindowBefore = readElement(application, kAXFocusedWindowAttribute) else { return nil }
        onStage(.focus_system_receiver)
        guard let systemReceiverBefore = readElement(system, kAXFocusedUIElementAttribute) else { return nil }
        onStage(.focus_app_receiver)
        guard let applicationReceiverBefore = readElement(application, kAXFocusedUIElementAttribute) else { return nil }
        onStage(.focus_receiver_match)
        guard CFEqual(systemReceiverBefore, applicationReceiverBefore) else { return nil }
        onStage(.focus_windows)
        guard let windows = readElements(application, kAXWindowsAttribute, maximum: 32) else { return nil }
        onStage(.window)
        guard let selected = uniqueRegisteredWindow(windows, application: application,
                  system: system, focusedWindow: focusedWindowBefore) else { return nil }
        onStage(.tree)
        guard let firstTree = scanWindow(selected.window),
              firstTree.receiverMatches(systemReceiverBefore) == 1,
              ancestryRolesMatch(firstTree, receiver: systemReceiverBefore) else { return nil }
        onStage(.receiver)
        guard let receiverFact = receiverFacts(systemReceiverBefore),
              receiverFact.role == kAXTextAreaRole,
              receiverFact.subrole == "" || receiverFact.subroleUnsupported,
              receiverFact.enabled == true || receiverFact.enabledUnsupported,
              receiverFact.focused == true, receiverFact.valueSettable,
              !receiverFact.secure else { return nil }
        onStage(.frame)
        guard let receiverFrameBefore = readFrame(systemReceiverBefore),
              proof.receiver.receiver_frame.matches(receiverFrameBefore),
              let windowFrameBefore = readFrame(selected.window),
              proof.receiver.window_frame.matches(windowFrameBefore) else { return nil }
        onStage(.marked)
        guard let markedBefore = readMarkedRange(systemReceiverBefore), markedBefore.unmarked,
              beforeDeadline(), liveProcess() else { return nil }

        onStage(.revalidation)
        guard let focusedApplicationAfter = readElement(system, kAXFocusedApplicationAttribute),
              CFEqual(focusedApplicationAfter, application),
              let focusedWindowAfter = readElement(application, kAXFocusedWindowAttribute),
              CFEqual(focusedWindowAfter, selected.window),
              let windowsAfter = readElements(application, kAXWindowsAttribute, maximum: 32),
              let selectedAfter = uniqueRegisteredWindow(windowsAfter, application: application,
                  system: system, focusedWindow: focusedWindowAfter),
              CFEqual(selectedAfter.window, selected.window),
              selectedAfter.windowID == selected.windowID,
              selectedAfter.documentPath == selected.documentPath,
              let systemReceiverAfter = readElement(system, kAXFocusedUIElementAttribute),
              let applicationReceiverAfter = readElement(application, kAXFocusedUIElementAttribute),
              CFEqual(systemReceiverAfter, systemReceiverBefore),
              CFEqual(applicationReceiverAfter, systemReceiverBefore),
              let secondTree = scanWindow(selected.window),
              secondTree.receiverMatches(systemReceiverBefore) == 1,
              ancestryRolesMatch(secondTree, receiver: systemReceiverBefore),
              let receiverFactAfter = receiverFacts(systemReceiverBefore),
              receiverFactAfter.role == receiverFact.role,
              receiverFactAfter.subrole == receiverFact.subrole,
              receiverFactAfter.subroleUnsupported == receiverFact.subroleUnsupported,
              receiverFactAfter.enabled == receiverFact.enabled,
              receiverFactAfter.enabledUnsupported == receiverFact.enabledUnsupported,
              receiverFactAfter.focused == true, receiverFactAfter.valueSettable,
              !receiverFactAfter.secure,
              let markedAfter = readMarkedRange(systemReceiverBefore),
              markedAfter == markedBefore, markedAfter.unmarked,
              let receiverFrameAfter = readFrame(systemReceiverBefore),
              receiverFrameAfter.matches(receiverFrameBefore),
              let windowFrameAfter = readFrame(selected.window),
              windowFrameAfter.matches(windowFrameBefore),
              let documentAfter = readDocumentPath(selected.window),
              documentAfter == registration.document_path,
              documentAfter == selected.documentPath,
              let cgWindowAfter = uniqueCGWindow(frame: windowFrameAfter, excludingDesktop: true),
              cgWindowAfter == selected.windowID,
              beforeDeadline(), liveProcess(),
              proof.isFresh(nowSourceMS: sourceNowMS(), maxAgeMS: maxAgeMS) else { return nil }
        guard let appAfter = NSRunningApplication(processIdentifier: pid), !appAfter.isTerminated,
              appAfter.bundleIdentifier == registration.bundle_id,
              ownedExecutablePath(registration.pid) == registration.executable_path else { return nil }
        return (systemReceiverBefore, selected.window)
    }

    private func uniqueRegisteredWindow(_ windows: [AXUIElement], application: AXUIElement,
                                        system: AXUIElement, focusedWindow: AXUIElement)
        -> (window: AXUIElement, windowID: UInt32, documentPath: String)? {
        onStage(.window_membership)
        // Match Source's owned profile: every current window must be covered
        // by the independent full-tree scans, so exactly one is permitted.
        guard windows.count == 1,
              windows.filter({ CFEqual($0, focusedWindow) }).count == 1 else { return nil }
        var matched: [(AXUIElement, UInt32, String)] = []
        for window in windows {
            onStage(.window_attributes)
            guard beforeDeadline(), setTimeout(window), processMatches(window),
                  let role = readText(window, kAXRoleAttribute), role == kAXWindowRole,
                  let subrole = readText(window, kAXSubroleAttribute), subrole == kAXStandardWindowSubrole,
                  let modal = readBoolean(window, kAXModalAttribute), !modal else { return nil }
            onStage(.window_document)
            guard let documentPath = readDocumentPath(window) else { return nil }
            onStage(.window_sheets)
            // Unsupported AXSheets is not evidence of an empty list. Both
            // paths still require the complete, non-transient tree scans.
            guard readSheetInventory(window) != nil else { return nil }
            if documentPath == registration.document_path {
                onStage(.window_frame)
                guard let frame = readFrame(window), proof.receiver.window_frame.matches(frame),
                      let windowID = uniqueCGWindow(frame: frame, excludingDesktop: true),
                      proof.receiver.window_id == "window-\(windowID)" else { return nil }
                matched.append((window, windowID, documentPath))
            }
        }
        onStage(.window_match)
        guard matched.count == 1,
              CFEqual(matched[0].0, focusedWindow),
              let focusedApplication = readElement(system, kAXFocusedApplicationAttribute),
              CFEqual(focusedApplication, application) else { return nil }
        return (matched[0].0, matched[0].1, matched[0].2)
    }

    private func scanWindow(_ window: AXUIElement) -> OwnedDocumentAXTree? {
        guard beforeDeadline() else { return nil }
        var nodes: [OwnedDocumentAXNode] = []
        var queue: [(AXUIElement, Int?, Int)] = [(window, nil, 0)]
        var cursor = 0
        while cursor < queue.count {
            guard beforeDeadline(), queue.count <= 512, cursor < queue.count else { return nil }
            let (element, parent, depth) = queue[cursor]
            guard depth <= 16, processMatches(element),
                  let role = readText(element, kAXRoleAttribute) else { return nil }
            guard role != kAXWindowRole || parent == nil else { return nil }
            cursor += 1
            let modalRead = readRaw(element, kAXModalAttribute)
            let modalType = modalRead.value.map { UInt64(CFGetTypeID($0)) }
            let modalValue = modalRead.value.flatMap { value -> Bool? in
                guard CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
                return CFBooleanGetValue(unsafeDowncast(value, to: CFBoolean.self))
            }
            guard ownedNodeIsNonTransient(role: role,
                modalError: modalRead.error?.rawValue ?? Int32.min,
                modalTypeID: modalType, modalValue: modalValue, timely: modalRead.timely) else { return nil }
            let needsSubrole = [kAXTextAreaRole, kAXTextFieldRole, "AXSecureTextField"].contains(role)
            let subroleRead = needsSubrole
                ? readOptionalText(element, kAXSubroleAttribute)
                : (value: Optional<String>.none, unsupported: false, timely: true)
            guard subroleRead.timely else { return nil }
            let subrole = subroleRead.value
            if role == "AXSecureTextField" || role == kAXTextFieldRole || role == kAXComboBoxRole
                || subrole == kAXSecureTextFieldSubrole { return nil }
            if role.hasPrefix("AXText"), ![kAXTextAreaRole, kAXTextFieldRole].contains(role) {
                return nil
            }
            let index = nodes.count
            nodes.append(OwnedDocumentAXNode(element: element, parentIndex: parent,
                role: role, subrole: subroleRead.value, subroleUnsupported: subroleRead.unsupported,
                enabled: nil, enabledUnsupported: false, focused: nil, valueSettable: false, secure: false))
            guard let children = readChildren(element) else { return nil }
            if case .array(let childrenValue) = children {
                guard nodes.count + queue.count - cursor + childrenValue.count <= 512 else { return nil }
                for (childIndex, child) in childrenValue.enumerated() {
                    guard !nodes.contains(where: { CFEqual($0.element, child) }),
                          !queue[cursor...].contains(where: { CFEqual($0.0, child) }),
                          !childrenValue[..<childIndex].contains(where: { CFEqual($0, child) }) else { return nil }
                }
                for child in childrenValue { queue.append((child, index, depth + 1)) }
            }
        }
        guard cursor == queue.count, !nodes.isEmpty,
              nodes[0].role == kAXWindowRole, CFEqual(nodes[0].element, window) else { return nil }
        // The focused object must occur exactly once in the complete bounded
        // AXChildren traversal. Source refs are deliberately not consulted.
        return OwnedDocumentAXTree(nodes: nodes, window: window)
    }

    private func beforeDeadline() -> Bool { monotonicMS() < deadlineMS }

    private func liveProcess() -> Bool {
        beforeDeadline() && DeviceProcessStamp.read(pid) == expectedStamp
            && NSRunningApplication(processIdentifier: pid)?.bundleIdentifier == registration.bundle_id
            && ownedExecutablePath(registration.pid) == registration.executable_path
    }

    private func processMatches(_ element: AXUIElement) -> Bool {
        guard beforeDeadline() else { return false }
        var owner: pid_t = 0
        return AXUIElementGetPid(element, &owner) == .success && owner == pid && beforeDeadline()
    }

    private func setTimeout(_ element: AXUIElement) -> Bool {
        let now = monotonicMS()
        guard now < deadlineMS else { return false }
        let milliseconds = min(UInt64(40), deadlineMS - now)
        return AXUIElementSetMessagingTimeout(element, Float(milliseconds) / 1000) == .success
            && beforeDeadline()
    }

    private func readRaw(_ element: AXUIElement, _ attribute: String)
        -> (value: CFTypeRef?, error: AXError?, timely: Bool) {
        guard setTimeout(element) else { return (nil, nil, false) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        return (value, error, beforeDeadline())
    }

    private func readElement(_ element: AXUIElement, _ attribute: String) -> AXUIElement? {
        let read = readRaw(element, attribute)
        guard read.timely, read.error == .success, let value = read.value,
              CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return unsafeDowncast(value, to: AXUIElement.self)
    }

    private func readElements(_ element: AXUIElement, _ attribute: String, maximum: Int)
        -> [AXUIElement]? {
        let read = readRaw(element, attribute)
        guard read.timely, read.error == .success, let raw = read.value,
              CFGetTypeID(raw) == CFArrayGetTypeID(),
              let values = raw as? [AXUIElement],
              values.count == CFArrayGetCount(unsafeDowncast(raw, to: CFArray.self)),
              values.count <= maximum,
              values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else { return nil }
        return values
    }

    private func readChildren(_ element: AXUIElement) -> OwnedDocumentChildrenRead? {
        let read = readRaw(element, kAXChildrenAttribute)
        guard read.timely else { return nil }
        let typeID = read.value.map(CFGetTypeID).map(UInt64.init)
        let rawCount = read.value.flatMap { value -> Int? in
            guard CFGetTypeID(value) == CFArrayGetTypeID() else { return nil }
            return CFArrayGetCount(unsafeDowncast(value, to: CFArray.self))
        }
        guard let disposition = ownedNativeChildListDisposition(
            errorCode: read.error?.rawValue ?? Int32.min, cfTypeID: typeID, rawCount: rawCount
        ) else { return nil }
        switch disposition {
        case .noValueLeaf:
            guard read.value == nil else { return nil }
            return .noValueLeaf
        case .typedArray(let expectedCount):
            guard let raw = read.value, let values = raw as? [AXUIElement],
                  values.count == expectedCount,
                  values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else { return nil }
            return .array(values)
        }
    }

    private func readSheetInventory(_ element: AXUIElement) -> OwnedSheetInventoryDisposition? {
        let read = readRaw(element, "AXSheets")
        let typeID = read.value.map { UInt64(CFGetTypeID($0)) }
        let count = read.value.flatMap { value -> Int? in
            guard CFGetTypeID(value) == CFArrayGetTypeID() else { return nil }
            return CFArrayGetCount(unsafeDowncast(value, to: CFArray.self))
        }
        let convertedCount = read.value.flatMap { value -> Int? in
            guard CFGetTypeID(value) == CFArrayGetTypeID(), let values = value as? [AXUIElement],
                  values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else { return nil }
            return values.count
        }
        return ownedSheetInventoryDisposition(error: read.error?.rawValue ?? Int32.min,
            typeID: typeID, rawCount: count, convertedCount: convertedCount, timely: read.timely)
    }

    private func readText(_ element: AXUIElement, _ attribute: String) -> String? {
        let read = readRaw(element, attribute)
        guard read.timely, read.error == .success, let value = read.value,
              CFGetTypeID(value) == CFStringGetTypeID() else { return nil }
        return value as? String
    }

    private func readOptionalText(_ element: AXUIElement, _ attribute: String)
        -> (value: String?, unsupported: Bool, timely: Bool) {
        let read = readRaw(element, attribute)
        guard read.timely else { return (nil, false, false) }
        if read.error == .attributeUnsupported { return (nil, true, true) }
        guard read.error == .success, let value = read.value,
              CFGetTypeID(value) == CFStringGetTypeID(), let text = value as? String else {
            return (nil, false, false)
        }
        return (text, false, true)
    }

    private func readBoolean(_ element: AXUIElement, _ attribute: String) -> Bool? {
        let read = readRaw(element, attribute)
        guard read.timely, read.error == .success, let value = read.value,
              CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
        return CFBooleanGetValue(unsafeDowncast(value, to: CFBoolean.self))
    }

    private func readEnabled(_ element: AXUIElement) -> (value: Bool?, unsupported: Bool) {
        let read = readRaw(element, kAXEnabledAttribute)
        guard read.timely else { return (nil, false) }
        if read.error == .attributeUnsupported { return (nil, true) }
        guard read.error == .success, let value = read.value,
              CFGetTypeID(value) == CFBooleanGetTypeID() else { return (nil, false) }
        return (CFBooleanGetValue(unsafeDowncast(value, to: CFBoolean.self)), false)
    }

    private func isValueSettable(_ element: AXUIElement) -> Bool {
        guard setTimeout(element), beforeDeadline() else { return false }
        var settable = DarwinBoolean(false)
        let result = AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable)
        return result == .success && settable.boolValue && beforeDeadline()
    }

    private func receiverFacts(_ element: AXUIElement) -> OwnedDocumentAXNode? {
        guard beforeDeadline(), let role = readText(element, kAXRoleAttribute) else { return nil }
        let subrole = readOptionalText(element, kAXSubroleAttribute)
        guard subrole.timely, role == kAXTextAreaRole,
              subrole.value == "" || subrole.unsupported,
              let focused = readBoolean(element, kAXFocusedAttribute), focused,
              isValueSettable(element) else { return nil }
        let enabled = readEnabled(element)
        guard enabled.value == true || enabled.unsupported,
              beforeDeadline(), processMatches(element) else { return nil }
        return OwnedDocumentAXNode(element: element, parentIndex: nil,
            role: role, subrole: subrole.value, subroleUnsupported: subrole.unsupported,
            enabled: enabled.value, enabledUnsupported: enabled.unsupported,
            focused: focused, valueSettable: true, secure: false)
    }

    private func ancestryRolesMatch(_ tree: OwnedDocumentAXTree, receiver: AXUIElement) -> Bool {
        guard tree.receiverMatches(receiver) == 1,
              let index = tree.nodes.firstIndex(where: { CFEqual($0.element, receiver) }) else { return false }
        var expectedRoles: [String] = []
        var child = tree.nodes[index]
        var cursor = tree.nodes[index].parentIndex
        var visited = Set<Int>()
        while let parentIndex = cursor {
            guard visited.insert(parentIndex).inserted, parentIndex < tree.nodes.count else { return false }
            let parent = tree.nodes[parentIndex]
            guard let actualParent = readElement(child.element, kAXParentAttribute),
                  CFEqual(actualParent, parent.element) else { return false }
            expectedRoles.append(parent.role)
            if CFEqual(parent.element, tree.window) { break }
            child = parent
            cursor = parent.parentIndex
        }
        guard expectedRoles.last == kAXWindowRole,
              expectedRoles.count == proof.receiver.ancestry.count else { return false }
        return zip(expectedRoles, proof.receiver.ancestry).allSatisfy { $0 == $1.role }
    }

    private func readFrame(_ element: AXUIElement) -> HitRect? {
        let positionRead = readRaw(element, kAXPositionAttribute)
        let sizeRead = readRaw(element, kAXSizeAttribute)
        guard positionRead.timely, sizeRead.timely,
              positionRead.error == .success, sizeRead.error == .success,
              let position = positionRead.value, CFGetTypeID(position) == AXValueGetTypeID(),
              let size = sizeRead.value, CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        let pointValue = unsafeDowncast(position, to: AXValue.self)
        let sizeValue = unsafeDowncast(size, to: AXValue.self)
        guard AXValueGetType(pointValue) == .cgPoint, AXValueGetType(sizeValue) == .cgSize else { return nil }
        var point = CGPoint.zero
        var dimensions = CGSize.zero
        guard AXValueGetValue(pointValue, .cgPoint, &point),
              AXValueGetValue(sizeValue, .cgSize, &dimensions) else { return nil }
        let result = HitRect(CGRect(origin: point, size: dimensions))
        return result.valid && beforeDeadline() ? result : nil
    }

    private func readMarkedRange(_ element: AXUIElement) -> NativeOwnedTextRange? {
        let read = readRaw(element, NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue)
        guard read.timely, read.error == .success, let raw = read.value,
              CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
        let value = unsafeDowncast(raw, to: AXValue.self)
        guard AXValueGetType(value) == .cfRange else { return nil }
        var range = CFRange(location: 0, length: 0)
        guard AXValueGetValue(value, .cfRange, &range), beforeDeadline() else { return nil }
        return NativeOwnedTextRange(location: Int64(range.location), length: Int64(range.length))
    }

    private func readDocumentPath(_ element: AXUIElement) -> String? {
        let read = readRaw(element, kAXDocumentAttribute)
        guard read.timely, read.error == .success, let raw = read.value else { return nil }
        let url: URL?
        if CFGetTypeID(raw) == CFURLGetTypeID() {
            url = unsafeDowncast(raw, to: CFURL.self) as URL
        } else if CFGetTypeID(raw) == CFStringGetTypeID(), let text = raw as? String {
            if text.hasPrefix("/") { url = URL(fileURLWithPath: text) }
            else { url = URL(string: text) }
        } else { return nil }
        guard let url, url.isFileURL, url.query == nil, url.fragment == nil,
              url.user == nil, url.password == nil, url.port == nil,
              (url.host == nil || url.host?.isEmpty == true
                || url.host?.caseInsensitiveCompare("localhost") == .orderedSame),
              let canonical = ownedCanonicalPath(url.path), beforeDeadline() else { return nil }
        return canonical
    }

    private func uniqueCGWindow(frame: HitRect, excludingDesktop: Bool) -> UInt32? {
        guard beforeDeadline(),
              let list = CGWindowListCopyWindowInfo(
                excludingDesktop ? [.optionOnScreenOnly, .excludeDesktopElements] : [.optionAll, .excludeDesktopElements],
                kCGNullWindowID
              ) as? [[String: Any]] else { return nil }
        let matches = list.compactMap { info -> UInt32? in
            guard (info[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid,
                  (info[kCGWindowLayer as String] as? NSNumber)?.intValue == 0,
                  let id = (info[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
                  id > 0,
                  let bounds = info[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary),
                  HitRect(rect).matches(frame) else { return nil }
            return id
        }
        guard beforeDeadline(), matches.count == 1 else { return nil }
        return matches[0]
    }
}

private struct OwnedDocumentAXNode {
    let element: AXUIElement
    let parentIndex: Int?
    let role: String
    let subrole: String?
    let subroleUnsupported: Bool
    let enabled: Bool?
    let enabledUnsupported: Bool
    let focused: Bool?
    let valueSettable: Bool
    let secure: Bool
}

private enum OwnedDocumentChildrenRead {
    case noValueLeaf
    case array([AXUIElement])
}

private struct OwnedDocumentAXTree {
    let nodes: [OwnedDocumentAXNode]
    let window: AXUIElement

    func receiverMatches(_ receiver: AXUIElement) -> Int {
        nodes.reduce(into: 0) { count, node in
            if CFEqual(node.element, receiver) { count += 1 }
        }
    }

}

enum OwnedNativeChildListDisposition: Equatable {
    case noValueLeaf
    case typedArray(count: Int)
}

/// Keeps AXError.noValue distinct from a successful empty AXChildren array.
func ownedNativeChildListDisposition(errorCode: Int32, cfTypeID: UInt64?, rawCount: Int?)
    -> OwnedNativeChildListDisposition? {
    if errorCode == AXError.noValue.rawValue, cfTypeID == nil, rawCount == nil {
        return .noValueLeaf
    }
    if errorCode == AXError.success.rawValue,
       cfTypeID == UInt64(CFArrayGetTypeID()),
       let rawCount, (0...512).contains(rawCount) {
        return .typedArray(count: rawCount)
    }
    return nil
}

private func strictOwnedDocumentProofShape(_ raw: Any) -> Bool {
    guard let proof = ownedExactDictionary(raw, keys: NativeOwnedDocumentBindingProof.wireKeys),
          let surface = ownedExactDictionary(proof["surface"] as Any, keys: ["id", "epoch"]),
          let receiver = ownedExactDictionary(proof["receiver"] as Any, keys: NativeOwnedDocumentReceiverEvidence.wireKeys),
          let provenance = ownedExactDictionary(receiver["provenance"] as Any,
              keys: ["identity_boundary", "snapshot_id", "source_sequence", "observed_ms", "valid_until_ms", "clock_domain"]),
          ownedExactDictionary(provenance["identity_boundary"] as Any, keys: ["namespace", "generation"]) != nil,
          ownedExactDictionary(receiver["process"] as Any, keys: ["pid", "start_seconds", "start_micros"]) != nil,
          ownedExactDictionary(receiver["window_frame"] as Any, keys: ["x", "y", "width", "height"]) != nil,
          ownedExactDictionary(receiver["receiver_frame"] as Any, keys: ["x", "y", "width", "height"]) != nil,
          let ancestors = receiver["ancestry"] as? [Any],
          ancestors.allSatisfy({ ownedExactDictionary($0, keys: ["object_ref", "role"]) != nil }),
          let docBinding = ownedExactDictionary(receiver["document_binding"] as Any, keys: ["before", "after"]),
          ownedDocumentReadShape(docBinding["before"] as Any),
          ownedDocumentReadShape(docBinding["after"] as Any),
          ownedAXObservationShape(receiver["title"] as Any, valueKind: .string),
          ownedAXObservationShape(receiver["role"] as Any, valueKind: .string),
          ownedAXObservationShape(receiver["subrole"] as Any, valueKind: .string),
          ownedAXObservationShape(receiver["enabled"] as Any, valueKind: .boolean),
          ownedAXObservationShape(receiver["focused"] as Any, valueKind: .boolean),
          ownedAXObservationShape(receiver["value_settable"] as Any, valueKind: .boolean),
          ownedAXObservationShape(receiver["description"] as Any, valueKind: .string),
          ownedMarkedRangeShape(receiver["marked_range_before"] as Any),
          ownedMarkedRangeShape(receiver["marked_range_after"] as Any) else { return false }
    _ = surface
    if let readback = receiver["value_readback"], !(readback is NSNull) {
        guard ownedExactDictionary(readback, keys: [
            "receiver_object_ref", "receiver_same_cf_as_actual_object_ref", "observed_ms", "sha256", "utf8_byte_count",
        ]) != nil else { return false }
    }
    return true
}

private func ownedDocumentReadShape(_ value: Any) -> Bool {
    ownedExactDictionary(value, keys: ["status", "api_code", "cf_type_id", "observed_ms", "timely", "registered_path_matches"]) != nil
}

private func ownedMarkedRangeShape(_ value: Any) -> Bool {
    guard let read = ownedExactDictionary(value, keys: [
        "receiver_object_ref", "receiver_same_cf_as_actual_object_ref", "observed_ms", "observation",
    ]), let observation = read["observation"] else { return false }
    guard ownedAXObservationShape(observation, valueKind: .range),
          let observationObject = observation as? [String: Any],
          observationObject["status"] as? String == "value",
          let range = observationObject["value"] else { return false }
    return ownedExactDictionary(range, keys: ["location", "length"]) != nil
}

private enum OwnedAXJSONValueKind { case string, boolean, range }

private func ownedAXObservationShape(_ value: Any, valueKind: OwnedAXJSONValueKind? = nil) -> Bool {
    guard let fields = value as? [String: Any],
          let status = fields["status"] as? String else { return false }
    let keys: Set<String>
    switch status {
    case "value": keys = ["status", "value"]
    case "unsupported", "no_value", "failure": keys = ["status", "code"]
    case "decode_failure": keys = ["status", "api_code", "actual_cf_type_id"]
    case "not_attempted": keys = ["status", "reason"]
    default: return false
    }
    guard Set(fields.keys) == keys else { return false }
    switch status {
    case "value":
        guard let rawValue = fields["value"] else { return false }
        switch valueKind {
        case .string: return rawValue is String
        case .boolean: return ownedJSONBoolean(rawValue)
        case .range: return ownedExactDictionary(rawValue, keys: ["location", "length"]) != nil
        case nil: return true
        }
    case "unsupported": return ownedJSONInt32(fields["code"]) == AXError.attributeUnsupported.rawValue
    case "no_value": return ownedJSONInt32(fields["code"]) == AXError.noValue.rawValue
    case "failure":
        guard let code = ownedJSONInt32(fields["code"]) else { return false }
        return code != AXError.success.rawValue && code != AXError.attributeUnsupported.rawValue
            && code != AXError.noValue.rawValue
    case "decode_failure": return ownedJSONInt32(fields["api_code"]) == AXError.success.rawValue
    case "not_attempted": return (fields["reason"] as? String).map(nativeID) == true
    default: return false
    }
}

private func ownedExactDictionary(_ value: Any, keys: Set<String>) -> [String: Any]? {
    guard let dictionary = value as? [String: Any], Set(dictionary.keys) == keys else { return nil }
    return dictionary
}

private func ownedJSONBoolean(_ value: Any) -> Bool {
    guard let number = value as? NSNumber else { return false }
    return CFGetTypeID(number) == CFBooleanGetTypeID()
}

private func ownedJSONInt32(_ value: Any?) -> Int32? {
    guard let number = value as? NSNumber,
          CFGetTypeID(number) != CFBooleanGetTypeID(),
          number.doubleValue.isFinite,
          number.doubleValue.rounded(.towardZero) == number.doubleValue,
          number.doubleValue >= Double(Int32.min), number.doubleValue <= Double(Int32.max) else { return nil }
    return number.int32Value
}

private func ownedLowerHex(_ value: String, length: Int) -> Bool {
    value.utf8.count == length && value.utf8.allSatisfy {
        (48...57).contains($0) || (97...102).contains($0)
    }
}

private func ownedCanonicalPath(_ path: String) -> String? {
    guard let resolved = path.withCString({ realpath($0, nil) }) else { return nil }
    defer { free(resolved) }
    return String(cString: resolved)
}

private func ownedExecutablePath(_ pid: UInt32) -> String? {
    var bytes = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(Int32(pid), &bytes, UInt32(bytes.count)) > 0 else { return nil }
    let value = String(decoding: bytes.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self)
    return ownedCanonicalPath(value)
}

private func ownedReadBoundedFile(_ descriptor: Int32, expectedSize: off_t) -> Data? {
    guard expectedSize >= 0, expectedSize <= 4096 else { return nil }
    var bytes = [UInt8](repeating: 0, count: Int(expectedSize))
    var total = 0
    while total < bytes.count {
        let copied = bytes.withUnsafeMutableBytes { buffer in
            Darwin.read(descriptor, buffer.baseAddress!.advanced(by: total), buffer.count - total)
        }
        guard copied > 0 else { return nil }
        total += copied
    }
    return Data(bytes)
}

/// Foundation's JSON object decoders collapse duplicate dictionary keys. This
/// bounded token walk rejects duplicates before either decoder sees the bytes;
/// key strings are decoded so escaped spellings of the same key also collide.
func ownedJSONHasNoDuplicateKeys(_ data: Data) -> Bool {
    guard !data.isEmpty, data.count <= 64 * 1024 else { return false }
    var scanner = OwnedJSONDuplicateKeyScanner(bytes: Array(data))
    return scanner.scanDocument()
}

private struct OwnedJSONDuplicateKeyScanner {
    let bytes: [UInt8]
    var index = 0

    mutating func scanDocument() -> Bool {
        guard parseValue(depth: 0) else { return false }
        skipWhitespace()
        return index == bytes.count
    }

    private mutating func parseValue(depth: Int) -> Bool {
        guard depth <= 64 else { return false }
        skipWhitespace()
        guard index < bytes.count else { return false }
        switch bytes[index] {
        case 0x7B: return parseObject(depth: depth + 1)
        case 0x5B: return parseArray(depth: depth + 1)
        case 0x22: return parseStringRange() != nil
        case 0x74: return parseLiteral(Array("true".utf8))
        case 0x66: return parseLiteral(Array("false".utf8))
        case 0x6E: return parseLiteral(Array("null".utf8))
        case 0x2D, 0x30...0x39: return parseNumberToken()
        default: return false
        }
    }

    private mutating func parseObject(depth: Int) -> Bool {
        guard bytes[index] == 0x7B else { return false }
        index += 1
        skipWhitespace()
        if consume(0x7D) { return true }
        var keys = Set<String>()
        while keys.count < 8192 {
            skipWhitespace()
            guard let range = parseStringRange(),
                  let key = try? JSONDecoder().decode(String.self, from: Data(bytes[range])) ,
                  keys.insert(key).inserted else { return false }
            skipWhitespace()
            guard consume(0x3A), parseValue(depth: depth) else { return false }
            skipWhitespace()
            if consume(0x7D) { return true }
            guard consume(0x2C) else { return false }
        }
        return false
    }

    private mutating func parseArray(depth: Int) -> Bool {
        guard bytes[index] == 0x5B else { return false }
        index += 1
        skipWhitespace()
        if consume(0x5D) { return true }
        while true {
            guard parseValue(depth: depth) else { return false }
            skipWhitespace()
            if consume(0x5D) { return true }
            guard consume(0x2C) else { return false }
        }
    }

    private mutating func parseStringRange() -> Range<Int>? {
        guard index < bytes.count, bytes[index] == 0x22 else { return nil }
        let start = index
        index += 1
        while index < bytes.count {
            let byte = bytes[index]
            if byte == 0x22 {
                index += 1
                return start..<index
            }
            if byte < 0x20 { return nil }
            if byte == 0x5C {
                index += 1
                guard index < bytes.count else { return nil }
                if bytes[index] == 0x75 {
                    guard index + 4 < bytes.count else { return nil }
                    index += 5
                } else {
                    index += 1
                }
            } else { index += 1 }
        }
        return nil
    }

    private mutating func parseNumberToken() -> Bool {
        let start = index
        while index < bytes.count,
              ![0x20, 0x09, 0x0A, 0x0D, 0x2C, 0x5D, 0x7D].contains(bytes[index]) {
            index += 1
        }
        return index > start
    }

    private mutating func parseLiteral(_ literal: [UInt8]) -> Bool {
        guard index + literal.count <= bytes.count,
              Array(bytes[index..<(index + literal.count)]) == literal else { return false }
        index += literal.count
        return true
    }

    private mutating func skipWhitespace() {
        while index < bytes.count, [0x20, 0x09, 0x0A, 0x0D].contains(bytes[index]) { index += 1 }
    }

    private mutating func consume(_ byte: UInt8) -> Bool {
        guard index < bytes.count, bytes[index] == byte else { return false }
        index += 1
        return true
    }
}

private extension UInt64 {
    func saturatingSubtract(_ other: UInt64) -> UInt64 { self >= other ? self - other : 0 }
}
