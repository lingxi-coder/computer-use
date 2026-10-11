import Foundation
import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import CryptoKit

/// This evidence is a private, label-free scope sidecar. It does not alter the
/// generic Structure read, its completeness, or the session-wide AX observer.
enum OwnedDocumentReceiverProfile: String, Encodable, Sendable {
    case ownedPlainTextValueSetterV1 = "owned_plain_text_value_setter_v1"
}

enum OwnedDocumentAttributeStatus: String, Encodable, Sendable {
    case value
    case unsupported
    case noValue = "no_value"
    case failure
    case decodeFailure = "decode_failure"
    case notAttempted = "not_attempted"
}

enum OwnedDocumentNotAttemptedReason: String, Encodable, Sendable {
    case deadline
    case timeoutSetupFailed = "timeout_setup_failed"
}

private func invalidOwnedDocumentEncoding(_ value: Any, encoder: Encoder,
                                         reason: String) -> EncodingError {
    .invalidValue(value, EncodingError.Context(codingPath: encoder.codingPath,
        debugDescription: reason))
}

private func validateOwnedDocumentObservation(status: OwnedDocumentAttributeStatus,
                                              code: Int32?,
                                              reason: OwnedDocumentNotAttemptedReason?,
                                              hasValue: Bool?,
                                              encoder: Encoder,
                                              subject: Any) throws {
    switch status {
    case .value:
        if hasValue == false {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "value status has no value")
        }
    case .unsupported:
        guard code == AXError.attributeUnsupported.rawValue else {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "unsupported status has no exact AX error code")
        }
    case .noValue:
        guard code == AXError.noValue.rawValue else {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "no-value status has no exact AX error code")
        }
    case .failure:
        guard let code, code != AXError.success.rawValue,
              code != AXError.attributeUnsupported.rawValue, code != AXError.noValue.rawValue else {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "failure status has no non-success AX error code")
        }
    case .decodeFailure:
        guard code == AXError.success.rawValue else {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "decode failure lacks the actual API success code")
        }
    case .notAttempted:
        guard reason != nil else {
            throw invalidOwnedDocumentEncoding(subject, encoder: encoder, reason: "not-attempted status has no reason")
        }
    }
}

/// Worker-local AX reads retain raw status and timing separately from decoded
/// values. Only the typed observation projection can leave the AX worker.
struct OwnedDocumentAttributeObservation: Encodable, Sendable {
    let status: OwnedDocumentAttributeStatus
    let code: Int32?
    let actualCFTypeID: UInt64?
    let reason: OwnedDocumentNotAttemptedReason?

    private enum CodingKeys: String, CodingKey {
        case status, code, reason
        case apiCode = "api_code"
        case actualCFTypeID = "actual_cf_type_id"
    }

    func encode(to encoder: Encoder) throws {
        try validateOwnedDocumentObservation(status: status, code: code, reason: reason,
            hasValue: nil, encoder: encoder, subject: self)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(status, forKey: .status)
        switch status {
        case .value: break
        case .unsupported, .noValue, .failure:
            try container.encode(code!, forKey: .code)
        case .decodeFailure:
            try container.encode(code!, forKey: .apiCode)
            if let actualCFTypeID { try container.encode(actualCFTypeID, forKey: .actualCFTypeID) }
            else { try container.encodeNil(forKey: .actualCFTypeID) }
        case .notAttempted:
            try container.encode(reason!, forKey: .reason)
        }
    }
}

struct OwnedDocumentProperty<Value: Encodable & Sendable>: Encodable, Sendable {
    let status: OwnedDocumentAttributeStatus
    let value: Value?
    let code: Int32?
    let actualCFTypeID: UInt64?
    let reason: OwnedDocumentNotAttemptedReason?

    private enum CodingKeys: String, CodingKey {
        case status, value, code, reason
        case apiCode = "api_code"
        case actualCFTypeID = "actual_cf_type_id"
    }

    func encode(to encoder: Encoder) throws {
        try validateOwnedDocumentObservation(status: status, code: code, reason: reason,
            hasValue: value != nil, encoder: encoder, subject: self)
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(status, forKey: .status)
        switch status {
        case .value:
            try container.encode(value!, forKey: .value)
        case .unsupported, .noValue, .failure:
            try container.encode(code!, forKey: .code)
        case .decodeFailure:
            try container.encode(code!, forKey: .apiCode)
            if let actualCFTypeID { try container.encode(actualCFTypeID, forKey: .actualCFTypeID) }
            else { try container.encodeNil(forKey: .actualCFTypeID) }
        case .notAttempted:
            try container.encode(reason!, forKey: .reason)
        }
    }
}

struct OwnedDocumentAXRead<Value> {
    let value: Value?
    let observation: OwnedDocumentAttributeObservation
    let cfTypeID: CFTypeID?
    let returnedLate: Bool
    let timely: Bool
}

private func ownedObservation(status: OwnedDocumentAttributeStatus,
                              code: Int32? = nil,
                              actualCFTypeID: UInt64? = nil,
                              reason: OwnedDocumentNotAttemptedReason? = nil) -> OwnedDocumentAttributeObservation {
    OwnedDocumentAttributeObservation(status: status, code: code,
        actualCFTypeID: actualCFTypeID, reason: reason)
}

func ownedDocumentProperty<Value: Encodable & Sendable>(_ read: OwnedDocumentAXRead<Value>)
    -> OwnedDocumentProperty<Value> {
    OwnedDocumentProperty(status: read.observation.status, value: read.value,
        code: read.observation.code, actualCFTypeID: read.observation.actualCFTypeID,
        reason: read.observation.reason)
}

func projectOwnedDocumentAttribute<Value>(
    errorCode: Int32?,
    actualCFTypeID: CFTypeID?,
    expectedCFTypeID: CFTypeID,
    decodedValue: Value?,
    attempted: Bool,
    returnedLate: Bool,
    timeoutSetupErrorCode: Int32? = nil,
    notAttemptedReason: OwnedDocumentNotAttemptedReason = .deadline
) -> OwnedDocumentAXRead<Value> {
    guard attempted else {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .notAttempted, code: timeoutSetupErrorCode,
                                          reason: timeoutSetupErrorCode == nil ? notAttemptedReason : .timeoutSetupFailed),
            cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: false)
    }
    guard let errorCode else {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .failure), cfTypeID: actualCFTypeID,
            returnedLate: returnedLate, timely: false)
    }
    if errorCode == AXError.attributeUnsupported.rawValue {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .unsupported, code: errorCode),
            cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: !returnedLate)
    }
    if errorCode == AXError.noValue.rawValue {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .noValue, code: errorCode),
            cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: !returnedLate)
    }
    guard errorCode == AXError.success.rawValue else {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .failure, code: errorCode),
            cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: !returnedLate)
    }
    guard actualCFTypeID == expectedCFTypeID, let decodedValue else {
        return OwnedDocumentAXRead(value: nil,
            observation: ownedObservation(status: .decodeFailure, code: AXError.success.rawValue,
                actualCFTypeID: actualCFTypeID.map(UInt64.init)),
            cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: !returnedLate)
    }
    return OwnedDocumentAXRead(value: decodedValue,
        observation: ownedObservation(status: .value),
        cfTypeID: actualCFTypeID, returnedLate: returnedLate, timely: !returnedLate)
}

func projectOwnedDocumentURLAttribute(errorCode: Int32?, rawValue: CFTypeRef?,
                                      attempted: Bool, returnedLate: Bool,
                                      timeoutSetupErrorCode: Int32? = nil) -> OwnedDocumentAXRead<URL> {
    let typeID = rawValue.map(CFGetTypeID)
    guard attempted, errorCode == AXError.success.rawValue, let rawValue else {
        return projectOwnedDocumentAttribute(errorCode: errorCode,
            actualCFTypeID: typeID, expectedCFTypeID: CFURLGetTypeID(),
            decodedValue: Optional<URL>.none, attempted: attempted,
            returnedLate: returnedLate, timeoutSetupErrorCode: timeoutSetupErrorCode)
    }
    if typeID == CFURLGetTypeID() {
        let url = unsafeDowncast(rawValue, to: CFURL.self) as URL
        return projectOwnedDocumentAttribute(errorCode: errorCode,
            actualCFTypeID: typeID, expectedCFTypeID: CFURLGetTypeID(),
            decodedValue: url.isFileURL && url.baseURL == nil ? Optional(url) : nil,
            attempted: true, returnedLate: returnedLate)
    }
    if typeID == CFStringGetTypeID(), let string = rawValue as? String,
       let url = URL(string: string) {
        return projectOwnedDocumentAttribute(errorCode: errorCode,
            actualCFTypeID: typeID, expectedCFTypeID: CFStringGetTypeID(),
            decodedValue: url.isFileURL && url.baseURL == nil ? Optional(url) : nil,
            attempted: true, returnedLate: returnedLate)
    }
    return projectOwnedDocumentAttribute(errorCode: errorCode,
        actualCFTypeID: typeID, expectedCFTypeID: CFURLGetTypeID(),
        decodedValue: Optional<URL>.none, attempted: true, returnedLate: returnedLate)
}

/// Trusted Host registration is an in-process value, deliberately not Decodable
/// from NativeRequest or model output.
struct OwnedDocumentReceiverRegistration: Sendable, Equatable {
    let registrationID: String
    let pid: UInt32
    let process: ProcessStamp
    let executablePath: String
    let bundleID: String
    let documentCanonicalPath: String
    let documentSHA256: String
    let privateTemporaryDocument: Bool
    let plainTextDocument: Bool
    let targetWasNotPreexisting: Bool
    let processStartedWithinLaunchInterval: Bool
}

struct OwnedDocumentReceiverProvenance: Encodable, Sendable, Equatable {
    let identityBoundary: SourceIdentityBoundary?
    let snapshotID: String?
    let sourceSequence: UInt64
    let observedMS: UInt64
    let validUntilMS: UInt64
    let clockDomain: String

    private enum CodingKeys: String, CodingKey {
        case identityBoundary = "identity_boundary"
        case snapshotID = "snapshot_id"
        case sourceSequence = "source_sequence"
        case observedMS = "observed_ms"
        case validUntilMS = "valid_until_ms"
        case clockDomain = "clock_domain"
    }
}

/// Per-read source provenance reserved by the caller before the worker begins.
/// It carries no target identity; the AX worker binds the actual registry
/// boundary only after its own complete read and postchecks.
struct OwnedDocumentReadScope: Sendable, Equatable {
    let snapshotID: String
    let sourceSequence: UInt64
    let observedMS: UInt64
    let validUntilMS: UInt64
    let clockDomain: String

    func provenance(identityBoundary: SourceIdentityBoundary?,
                    readStartedMS: UInt64, readFinishedMS: UInt64) -> OwnedDocumentReceiverProvenance? {
        guard let identityBoundary,
              validOwnedDocumentRef(snapshotID), sourceSequence > 0,
              validOwnedDocumentRef(clockDomain), clockDomain.utf8.count <= 128,
              observedMS > 0, observedMS <= readStartedMS,
              readStartedMS <= readFinishedMS, readFinishedMS < validUntilMS else { return nil }
        return OwnedDocumentReceiverProvenance(identityBoundary: identityBoundary,
            snapshotID: snapshotID, sourceSequence: sourceSequence, observedMS: observedMS,
            validUntilMS: validUntilMS, clockDomain: clockDomain)
    }
}

struct OwnedDocumentProcessIdentity: Encodable, Sendable, Equatable {
    let pid: UInt32
    let startSeconds: UInt64
    let startMicros: UInt64

    private enum CodingKeys: String, CodingKey {
        case pid
        case startSeconds = "start_seconds"
        case startMicros = "start_micros"
    }
}

struct OwnedDocumentTextRange: Encodable, Sendable, Equatable {
    let location: Int
    let length: Int

    var valid: Bool {
        guard location >= 0, length >= 0 else { return false }
        let (_, overflow) = location.addingReportingOverflow(length)
        return !overflow
    }
}

struct OwnedDocumentTextMetadata: Encodable, Sendable, Equatable {
    let utf8Length: Int
    let unicodeScalarCount: Int
}

/// A bounded digest of the exact UTF-8 bytes returned by AXValue. The text is
/// never retained in, or encoded with, this readback.
struct OwnedDocumentValueReadback: Encodable, Sendable, Equatable {
    let receiver_object_ref: String
    let receiver_same_cf_as_actual_object_ref: Bool
    let observed_ms: UInt64
    let sha256: String
    let utf8_byte_count: UInt32
}

func boundedOwnedDocumentValue(_ raw: CFTypeRef) -> String? {
    guard CFGetTypeID(raw) == CFStringGetTypeID() else { return nil }
    let string = unsafeDowncast(raw, to: CFString.self)
    guard CFStringGetLength(string) <= 2_048,
          let text = raw as? String, text.utf8.count <= 2_048 else { return nil }
    return text
}

func projectOwnedDocumentValueReadback(
    _ read: OwnedDocumentAXRead<String>,
    receiverObjectRef: String,
    receiverSameCFAsActualObjectRef: Bool,
    observedMS: UInt64,
    validUntilMS: UInt64
) -> OwnedDocumentValueReadback? {
    guard read.observation.status == .value,
          read.timely, !read.returnedLate,
          read.cfTypeID == CFStringGetTypeID(),
          let text = read.value,
          validOwnedDocumentRef(receiverObjectRef),
          receiverSameCFAsActualObjectRef,
          observedMS > 0, observedMS < validUntilMS else { return nil }
    let byteCount = text.utf8.count
    guard byteCount <= 2_048 else { return nil }
    let digest = SHA256.hash(data: Data(text.utf8))
        .map { String(format: "%02x", $0) }
        .joined()
    return OwnedDocumentValueReadback(
        receiver_object_ref: receiverObjectRef,
        receiver_same_cf_as_actual_object_ref: true,
        observed_ms: observedMS,
        sha256: digest,
        utf8_byte_count: UInt32(byteCount))
}

func isLocalOwnedDocumentFileURL(_ url: URL) -> Bool {
    guard url.isFileURL, url.baseURL == nil,
          url.user == nil, url.password == nil, url.port == nil,
          url.query == nil, url.fragment == nil else { return false }
    return url.host == nil || url.host?.isEmpty == true
        || url.host?.caseInsensitiveCompare("localhost") == .orderedSame
}

struct OwnedDocumentAXElementArrayRead {
    let observation: OwnedDocumentAttributeObservation
    let elements: [MenuRetainedToken]
    let rawElementCount: Int?
    let returnedElementCount: Int
    let truncated: Bool
    let conversionSucceeded: Bool
    let returnedLate: Bool

    var complete: Bool {
        observation.status == .value && observation.code == nil && !returnedLate
            && conversionSucceeded && !truncated
            && rawElementCount == returnedElementCount
            && returnedElementCount == elements.count
    }

    func membershipCount(of element: CFTypeRef) -> Int {
        elements.reduce(into: 0) { count, candidate in
            if candidate.matches(element) { count += 1 }
        }
    }
}

/// Synchronous reader intended for the existing dedicated AX worker thread.
/// None of its CF references are Sendable or included in the sidecar DTO.
struct OwnedDocumentAXReader {
    let deadline: UInt64
    let maximumArrayElements: Int

    init(deadline: UInt64, maximumArrayElements: Int = 512) {
        self.deadline = deadline
        self.maximumArrayElements = max(1, min(maximumArrayElements, 512))
    }

    private func copy(_ element: AXUIElement, attribute: String)
        -> (value: CFTypeRef?, errorCode: Int32?, attempted: Bool,
            returnedLate: Bool, setupErrorCode: Int32?) {
        let before = uptimeNs()
        guard before < deadline else { return (nil, nil, false, false, nil) }
        let remaining = Double(deadline - before) / 1_000_000_000
        let timeout = Float(min(0.04, remaining))
        let setup = AXUIElementSetMessagingTimeout(element, timeout)
        guard setup == .success else { return (nil, nil, false, false, setup.rawValue) }
        guard uptimeNs() < deadline else { return (nil, nil, false, true, nil) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        let late = uptimeNs() >= deadline
        return (value, error.rawValue, true, late, nil)
    }

    private func typed<Value>(_ element: AXUIElement, attribute: String,
                              expectedTypeID: CFTypeID,
                              decode: (CFTypeRef) -> Value?) -> OwnedDocumentAXRead<Value> {
        let raw = copy(element, attribute: attribute)
        let typeID = raw.value.map(CFGetTypeID)
        let decoded = raw.value.flatMap { value in
            typeID == expectedTypeID ? decode(value) : nil
        }
        return projectOwnedDocumentAttribute(errorCode: raw.errorCode,
            actualCFTypeID: typeID, expectedCFTypeID: expectedTypeID,
            decodedValue: decoded, attempted: raw.attempted,
            returnedLate: raw.returnedLate, timeoutSetupErrorCode: raw.setupErrorCode)
    }

    func string(_ element: AXUIElement, attribute: String) -> OwnedDocumentAXRead<String> {
        typed(element, attribute: attribute, expectedTypeID: CFStringGetTypeID()) { $0 as? String }
    }

    func boundedValue(_ element: AXUIElement) -> OwnedDocumentAXRead<String> {
        typed(element, attribute: kAXValueAttribute, expectedTypeID: CFStringGetTypeID(),
              decode: boundedOwnedDocumentValue)
    }

    func boolean(_ element: AXUIElement, attribute: String) -> OwnedDocumentAXRead<Bool> {
        typed(element, attribute: attribute, expectedTypeID: CFBooleanGetTypeID()) {
            CFBooleanGetValue(unsafeDowncast($0, to: CFBoolean.self))
        }
    }

    func axElement(_ element: AXUIElement, attribute: String) -> OwnedDocumentAXRead<MenuRetainedToken> {
        typed(element, attribute: attribute, expectedTypeID: AXUIElementGetTypeID()) {
            MenuRetainedToken(unsafeDowncast($0, to: AXUIElement.self))
        }
    }

    func elements(_ element: AXUIElement, attribute: String,
                  maximum: Int? = nil) -> OwnedDocumentAXElementArrayRead {
        let raw = copy(element, attribute: attribute)
        let typeID = raw.value.map(CFGetTypeID)
        guard raw.attempted, raw.errorCode == AXError.success.rawValue,
              !raw.returnedLate, let value = raw.value,
              typeID == CFArrayGetTypeID() else {
            let projected = projectOwnedDocumentAttribute(errorCode: raw.errorCode,
                actualCFTypeID: typeID, expectedCFTypeID: CFArrayGetTypeID(),
                decodedValue: Optional<[MenuRetainedToken]>.none, attempted: raw.attempted,
                returnedLate: raw.returnedLate, timeoutSetupErrorCode: raw.setupErrorCode)
            return OwnedDocumentAXElementArrayRead(observation: projected.observation,
                elements: [], rawElementCount: nil, returnedElementCount: 0,
                truncated: false, conversionSucceeded: false, returnedLate: raw.returnedLate)
        }
        let array = unsafeDowncast(value, to: CFArray.self)
        let rawCount = CFArrayGetCount(array)
        guard let values = value as? [AXUIElement], values.count == rawCount,
              values.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }) else {
            let projected = projectOwnedDocumentAttribute(errorCode: AXError.success.rawValue,
                actualCFTypeID: typeID, expectedCFTypeID: CFArrayGetTypeID(),
                decodedValue: Optional<[MenuRetainedToken]>.none, attempted: true,
                returnedLate: false)
            return OwnedDocumentAXElementArrayRead(observation: projected.observation,
                elements: [], rawElementCount: rawCount, returnedElementCount: 0,
                truncated: false, conversionSucceeded: false, returnedLate: false)
        }
        let limit = max(0, min(maximum ?? maximumArrayElements, maximumArrayElements))
        let bounded = values.prefix(limit).map(MenuRetainedToken.init)
        let projected = projectOwnedDocumentAttribute(errorCode: AXError.success.rawValue,
            actualCFTypeID: typeID, expectedCFTypeID: CFArrayGetTypeID(),
            decodedValue: Optional(bounded), attempted: true, returnedLate: false)
        return OwnedDocumentAXElementArrayRead(observation: projected.observation,
            elements: Array(bounded), rawElementCount: rawCount,
            returnedElementCount: bounded.count, truncated: bounded.count < rawCount,
            conversionSucceeded: true, returnedLate: false)
    }

    func frame(_ element: AXUIElement) -> OwnedDocumentAXRead<Rect> {
        let position = typed(element, attribute: kAXPositionAttribute,
                             expectedTypeID: AXValueGetTypeID()) { raw -> CGPoint? in
            let value = unsafeDowncast(raw, to: AXValue.self)
            guard AXValueGetType(value) == .cgPoint else { return nil }
            var point = CGPoint.zero
            return AXValueGetValue(value, .cgPoint, &point) ? point : nil
        }
        let size = typed(element, attribute: kAXSizeAttribute,
                         expectedTypeID: AXValueGetTypeID()) { raw -> CGSize? in
            let value = unsafeDowncast(raw, to: AXValue.self)
            guard AXValueGetType(value) == .cgSize else { return nil }
            var size = CGSize.zero
            return AXValueGetValue(value, .cgSize, &size) ? size : nil
        }
        guard position.timely, size.timely,
              position.observation.status == .value, size.observation.status == .value,
              let point = position.value, let sizeValue = size.value else {
            if !position.timely || position.observation.status != .value {
                return OwnedDocumentAXRead<Rect>(value: nil, observation: position.observation,
                    cfTypeID: position.cfTypeID, returnedLate: position.returnedLate, timely: false)
            }
            return OwnedDocumentAXRead<Rect>(value: nil, observation: size.observation,
                cfTypeID: size.cfTypeID, returnedLate: size.returnedLate, timely: false)
        }
        let rect = Rect(CGRect(origin: point, size: sizeValue))
        guard rect.valid, rect.width > 0, rect.height > 0 else {
            return projectOwnedDocumentAttribute(errorCode: AXError.success.rawValue,
                actualCFTypeID: AXValueGetTypeID(), expectedCFTypeID: AXValueGetTypeID(),
                decodedValue: Optional<Rect>.none, attempted: true, returnedLate: false)
        }
        return OwnedDocumentAXRead(value: rect,
            observation: ownedObservation(status: .value), cfTypeID: AXValueGetTypeID(),
            returnedLate: false, timely: true)
    }

    func markedRange(_ element: AXUIElement) -> OwnedDocumentAXRead<OwnedDocumentTextRange> {
        typed(element, attribute: NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue,
              expectedTypeID: AXValueGetTypeID()) { raw -> OwnedDocumentTextRange? in
            let value = unsafeDowncast(raw, to: AXValue.self)
            guard AXValueGetType(value) == .cfRange else { return nil }
            var range = CFRange(location: 0, length: 0)
            guard AXValueGetValue(value, .cfRange, &range) else { return nil }
            let result = OwnedDocumentTextRange(location: range.location, length: range.length)
            return result.valid ? result : nil
        }
    }

    func valueIsSettable(_ element: AXUIElement) -> OwnedDocumentAXRead<Bool> {
        let before = uptimeNs()
        guard before < deadline else {
            return projectOwnedDocumentAttribute(errorCode: nil, actualCFTypeID: nil,
                expectedCFTypeID: CFBooleanGetTypeID(), decodedValue: Optional<Bool>.none,
                attempted: false, returnedLate: false)
        }
        let remaining = Double(deadline - before) / 1_000_000_000
        let timeoutSetup = AXUIElementSetMessagingTimeout(element, Float(min(0.04, remaining)))
        guard timeoutSetup == .success else {
            return projectOwnedDocumentAttribute(errorCode: nil, actualCFTypeID: nil,
                expectedCFTypeID: CFBooleanGetTypeID(), decodedValue: Optional<Bool>.none,
                attempted: false, returnedLate: uptimeNs() >= deadline,
                timeoutSetupErrorCode: timeoutSetup.rawValue)
        }
        guard uptimeNs() < deadline else {
            return projectOwnedDocumentAttribute(errorCode: nil, actualCFTypeID: nil,
                expectedCFTypeID: CFBooleanGetTypeID(), decodedValue: Optional<Bool>.none,
                attempted: false, returnedLate: true)
        }
        var settable = DarwinBoolean(false)
        let error = AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable)
        return projectOwnedDocumentAttribute(errorCode: error.rawValue,
            actualCFTypeID: error == .success ? CFBooleanGetTypeID() : nil,
            expectedCFTypeID: CFBooleanGetTypeID(), decodedValue: error == .success ? settable.boolValue : nil,
            attempted: true, returnedLate: uptimeNs() >= deadline)
    }

    func documentURL(_ element: AXUIElement) -> OwnedDocumentAXRead<URL> {
        let raw = copy(element, attribute: kAXDocumentAttribute)
        return projectOwnedDocumentURLAttribute(errorCode: raw.errorCode,
            rawValue: raw.value, attempted: raw.attempted, returnedLate: raw.returnedLate,
            timeoutSetupErrorCode: raw.setupErrorCode)
    }
}

enum OwnedDocumentObserverState: String, Encodable, Sendable {
    case continuous
    case coverageIncomplete = "coverage_incomplete"
    case changedDuringRead = "changed_during_read"
    case notAttempted = "not_attempted"
}

enum OwnedDocumentProjectionGate: String, Encodable, Sendable {
    case evidenceUnavailable = "evidence_unavailable"
    case observerNotContinuous = "observer_not_continuous"
    case scopeIncomplete = "scope_incomplete"
    case accepted
}

func ownedDocumentProjectionGate(_ evidence: OwnedDocumentReceiverEvidence?) -> OwnedDocumentProjectionGate {
    guard let evidence else { return .evidenceUnavailable }
    guard evidence.observer_state == .continuous else { return .observerNotContinuous }
    guard evidence.scope_complete else { return .scopeIncomplete }
    return .accepted
}

struct OwnedDocumentProjectionGateSummary: Encodable, Sendable {
    let registration_valid: Bool
    let provenance_valid: Bool
    let source_read_complete: Bool
    let document_binding_verified: Bool
    let process_valid: Bool
    let application_focus_valid: Bool
    let window_read_complete: Bool
    let window_profile_valid: Bool
    let receiver_focus_valid: Bool
    let ancestor_chain_valid: Bool
    let receiver_frames_valid: Bool
    let receiver_reads_complete: Bool
    let observer_state: OwnedDocumentObserverState
    let scope_complete: Bool
    let no_modal_or_transient: Bool
    let secure_receiver_known: Bool
    let receiver_is_secure: Bool?
    let required_identity_fields_valid: Bool
}

func ownedDocumentProjectionGateSummary(_ facts: OwnedDocumentReceiverReadFacts)
    -> OwnedDocumentProjectionGateSummary {
    let registrationValid = validOwnedDocumentRegistration(facts.registration)
    let provenanceValid = validOwnedDocumentProvenance(facts.provenance, nowMS: facts.nowMS)
    let processValid = facts.processBefore == facts.registration.process
        && facts.processAfter == facts.registration.process
        && facts.applicationPID == facts.registration.pid
        && facts.systemFocusedApplicationPID == facts.registration.pid
        && facts.observedBundleID == facts.registration.bundleID
        && facts.observedExecutablePath == facts.registration.executablePath
    let applicationFocusValid = facts.systemFocusedApplication.observation.status == .value
        && facts.systemFocusedApplication.timely
        && facts.systemFocusedApplication.value?.matches(facts.application.element) == true
    let windowReadComplete: Bool
    let windowProfileValid: Bool
    if let window = facts.window {
        let identified = identifiedNativeWindow(window)
        windowReadComplete = facts.actualWindows.complete
            && cfEqualCount(window.element, in: facts.actualWindows.elements) == 1
            && identified?.pid == facts.registration.pid && !window.objectRef.isEmpty
            && readIsTimely(window.role) && readIsTimely(window.subrole) && readIsTimely(window.modal)
        windowProfileValid = windowReadComplete && facts.actualWindows.elements.count == 1
            && stableNormalWindow(window) != nil
    } else {
        windowReadComplete = false
        windowProfileValid = false
    }
    let receiverFocusValid: Bool
    let ancestorChainValid: Bool
    if let window = facts.window, let receiver = facts.receiver {
        receiverFocusValid = receiverIdentityStable(facts, receiver: receiver, window: window)
            && receiver.pid == facts.registration.pid
        ancestorChainValid = validAncestorChain(facts, window: window, receiver: receiver,
            registration: facts.registration)
    } else {
        receiverFocusValid = false
        ancestorChainValid = false
    }
    let receiverFramesValid: Bool
    if let window = facts.window, let receiver = facts.receiver,
       let windowFrame = window.frameAfter.value,
       let receiverFrameBefore = receiver.frameBefore.value,
       let receiverFrameAfter = receiver.frameAfter.value {
        receiverFramesValid = window.frameBefore.timely && window.frameAfter.timely
            && receiver.frameBefore.timely && receiver.frameAfter.timely
            && receiverFrameBefore == receiverFrameAfter
            && receiverFrameAfter.valid && receiverFrameAfter.width > 0 && receiverFrameAfter.height > 0
            && windowFrame.cgRect.contains(receiverFrameAfter.cgRect)
    } else {
        receiverFramesValid = false
    }
    let rangeReadsComplete = readIsTimely(facts.markedRangeBefore.read)
        && readIsTimely(facts.markedRangeAfter.read)
        && facts.markedRangeBefore.observedMS >= facts.provenance.observedMS
        && facts.markedRangeBefore.observedMS <= facts.markedRangeAfter.observedMS
        && facts.markedRangeAfter.observedMS < facts.provenance.validUntilMS
        && facts.markedRangeAfter.observedMS <= facts.nowMS
    let receiverReadsComplete: Bool
    if let receiver = facts.receiver {
        receiverReadsComplete = readIsTimely(receiver.role) && readIsTimely(receiver.subrole)
            && readIsTimely(receiver.enabled) && readIsTimely(receiver.focused)
            && readIsTimely(receiver.valueSettable) && rangeReadsComplete
    } else {
        receiverReadsComplete = false
    }
    let observer: OwnedDocumentObserverState
    if let window = facts.window, let receiver = facts.receiver {
        observer = observerState(facts, window: window, receiver: receiver)
    } else {
        observer = .coverageIncomplete
    }
    let documentBindingVerified = facts.documentBindingVerified
        && facts.documentBinding.before.registered_path_matches
        && facts.documentBinding.after.registered_path_matches
    let scopeComplete = registrationValid && provenanceValid && facts.ownedScopeReadComplete
        && documentBindingVerified && processValid && applicationFocusValid && windowReadComplete
        && receiverFocusValid && ancestorChainValid && receiverFramesValid && receiverReadsComplete
        && observer == .continuous

    let processOutputValid: Bool
    if let processStamp = facts.processAfter, let observedPID = facts.applicationPID {
        processOutputValid = observedPID > 0 && observedPID <= UInt32(Int32.max)
            && processStamp.seconds > 0 && processStamp.micros < 1_000_000
            && facts.observedExecutablePath.map { !$0.isEmpty } == true
            && facts.observedBundleID.map { !$0.isEmpty } == true
    } else {
        processOutputValid = false
    }
    let windowOutputValid: Bool
    if let window = facts.window, let identified = identifiedNativeWindow(window),
       let frame = window.frameAfter.value {
        windowOutputValid = validOwnedDocumentRef(identified.ref)
            && validOwnedDocumentRef(window.objectRef) && validOwnedDocumentRect(frame)
    } else {
        windowOutputValid = false
    }
    let receiverOutputValid: Bool
    if let receiver = facts.receiver, let frame = receiver.frameAfter.value {
        receiverOutputValid = validOwnedDocumentRef(receiver.objectRef) && validOwnedDocumentRect(frame)
    } else {
        receiverOutputValid = false
    }
    let focusRefsValid = facts.applicationFocusedReceiverObjectRef.map(validOwnedDocumentRef) == true
        && facts.systemFocusedReceiverObjectRef.map(validOwnedDocumentRef) == true
    let ancestryOutputValid: Bool
    if let window = facts.window, let receiver = facts.receiver {
        ancestryOutputValid = ancestryEvidence(facts, window: window, receiver: receiver) != nil
    } else {
        ancestryOutputValid = false
    }
    let secure = facts.receiver.flatMap(secureReceiver)
    let requiredIdentityFieldsValid = processOutputValid && windowOutputValid && receiverOutputValid
        && focusRefsValid && ancestryOutputValid && secure != nil

    return OwnedDocumentProjectionGateSummary(registration_valid: registrationValid,
        provenance_valid: provenanceValid, source_read_complete: facts.ownedScopeReadComplete,
        document_binding_verified: documentBindingVerified, process_valid: processValid,
        application_focus_valid: applicationFocusValid, window_read_complete: windowReadComplete,
        window_profile_valid: windowProfileValid, receiver_focus_valid: receiverFocusValid,
        ancestor_chain_valid: ancestorChainValid, receiver_frames_valid: receiverFramesValid,
        receiver_reads_complete: receiverReadsComplete, observer_state: observer,
        scope_complete: scopeComplete, no_modal_or_transient: facts.noModalOrTransient,
        secure_receiver_known: secure != nil, receiver_is_secure: secure,
        required_identity_fields_valid: requiredIdentityFieldsValid)
}

enum OwnedDocumentObserverNotification: String, Sendable {
    case focusedWindowChanged = "focused_window_changed"
    case focusedElementChanged = "focused_element_changed"
    case windowCreated = "window_created"
    case applicationActivated = "application_activated"
    case applicationDeactivated = "application_deactivated"
    case elementDestroyed = "element_destroyed"
    case layoutChanged = "layout_changed"
    case windowMoved = "window_moved"
    case windowResized = "window_resized"

    init?(axName: String) {
        switch axName {
        case kAXFocusedWindowChangedNotification: self = .focusedWindowChanged
        case kAXFocusedUIElementChangedNotification: self = .focusedElementChanged
        case kAXWindowCreatedNotification: self = .windowCreated
        case kAXApplicationActivatedNotification: self = .applicationActivated
        case kAXApplicationDeactivatedNotification: self = .applicationDeactivated
        case kAXUIElementDestroyedNotification: self = .elementDestroyed
        case kAXLayoutChangedNotification: self = .layoutChanged
        case kAXWindowMovedNotification: self = .windowMoved
        case kAXWindowResizedNotification: self = .windowResized
        default: return nil
        }
    }

    var axName: String {
        switch self {
        case .focusedWindowChanged: return kAXFocusedWindowChangedNotification
        case .focusedElementChanged: return kAXFocusedUIElementChangedNotification
        case .windowCreated: return kAXWindowCreatedNotification
        case .applicationActivated: return kAXApplicationActivatedNotification
        case .applicationDeactivated: return kAXApplicationDeactivatedNotification
        case .elementDestroyed: return kAXUIElementDestroyedNotification
        case .layoutChanged: return kAXLayoutChangedNotification
        case .windowMoved: return kAXWindowMovedNotification
        case .windowResized: return kAXWindowResizedNotification
        }
    }
}

struct OwnedDocumentObserverRegistrationRead {
    let element: MenuRetainedToken
    let notification: OwnedDocumentObserverNotification
    let attempted: Bool
    let errorCode: Int32?
    let returnedLate: Bool

    var coversRequiredNotification: Bool {
        attempted && !returnedLate
            && (errorCode == AXError.success.rawValue
                || errorCode == AXError.notificationAlreadyRegistered.rawValue)
    }
}

struct OwnedDocumentObserverScopeRead {
    let sameWatch: Bool
    let observerAvailableBefore: Bool
    let observerAvailableAfter: Bool
    let generationBefore: UInt64?
    let generationAfter: UInt64?
    let revisionBefore: UInt64?
    let revisionAfter: UInt64?
    let registrations: [OwnedDocumentObserverRegistrationRead]

    func state(application: MenuRetainedToken, window: MenuRetainedToken,
               receiver: MenuRetainedToken, ancestors: [MenuRetainedToken]) -> OwnedDocumentObserverState {
        guard let generationBefore, let generationAfter, let revisionBefore, let revisionAfter else {
            return .notAttempted
        }
        guard generationBefore == generationAfter, revisionBefore == revisionAfter else {
            return .changedDuringRead
        }
        guard sameWatch, observerAvailableBefore, observerAvailableAfter else { return .coverageIncomplete }

        let applicationNotifications: [OwnedDocumentObserverNotification] = [
            .focusedWindowChanged, .focusedElementChanged, .windowCreated,
            .applicationActivated, .applicationDeactivated
        ]
        let windowNotifications: [OwnedDocumentObserverNotification] = [
            .elementDestroyed, .layoutChanged, .windowMoved, .windowResized
        ]
        func hasExactlyOne(_ target: MenuRetainedToken,
                           _ notification: OwnedDocumentObserverNotification) -> Bool {
            let matching = registrations.filter {
                $0.notification == notification && $0.element.matches(target.element)
            }
            return matching.count == 1 && matching[0].coversRequiredNotification
        }
        guard applicationNotifications.allSatisfy({ hasExactlyOne(application, $0) }),
              windowNotifications.allSatisfy({ hasExactlyOne(window, $0) }),
              hasExactlyOne(receiver, .elementDestroyed),
              ancestors.allSatisfy({ hasExactlyOne($0, .elementDestroyed) }) else {
            return .coverageIncomplete
        }
        return .continuous
    }
}

struct OwnedDocumentReceiverWindowRead {
    let element: MenuRetainedToken
    let objectRef: String
    let role: OwnedDocumentAXRead<String>
    let subrole: OwnedDocumentAXRead<String>
    let modal: OwnedDocumentAXRead<Bool>
    let frameBefore: OwnedDocumentAXRead<Rect>
    let frameAfter: OwnedDocumentAXRead<Rect>
    let cgWindowsBefore: [NativeWindow]
    let cgWindowsAfter: [NativeWindow]
}

struct OwnedDocumentReceiverElementRead {
    let element: MenuRetainedToken
    let objectRef: String
    let pid: UInt32?
    let frameBefore: OwnedDocumentAXRead<Rect>
    let frameAfter: OwnedDocumentAXRead<Rect>
    let role: OwnedDocumentAXRead<String>
    let subrole: OwnedDocumentAXRead<String>
    let enabled: OwnedDocumentAXRead<Bool>
    let focused: OwnedDocumentAXRead<Bool>
    let valueSettable: OwnedDocumentAXRead<Bool>
    let title: OwnedDocumentAXRead<String>
    let description: OwnedDocumentAXRead<String>
}

struct OwnedDocumentAncestorRelationRead {
    let child: MenuRetainedToken
    let parent: MenuRetainedToken
    let parentAttribute: OwnedDocumentAXRead<MenuRetainedToken>
    let parentChildren: OwnedDocumentAXElementArrayRead
    let parentPID: UInt32?
    let parentObjectRef: String
    let parentRole: OwnedDocumentAXRead<String>
}

struct OwnedDocumentMarkedRangeRead {
    let receiver: MenuRetainedToken
    let observedMS: UInt64
    let read: OwnedDocumentAXRead<OwnedDocumentTextRange>
}

struct OwnedDocumentReceiverReadFacts {
    let registration: OwnedDocumentReceiverRegistration
    let documentBindingVerified: Bool
    let documentBinding: OwnedDocumentPathBindingEvidence
    let provenance: OwnedDocumentReceiverProvenance
    let nowMS: UInt64
    let ownedScopeReadComplete: Bool
    let noModalOrTransient: Bool
    let application: MenuRetainedToken
    let applicationPID: UInt32?
    let systemFocusedApplication: OwnedDocumentAXRead<MenuRetainedToken>
    let systemFocusedApplicationPID: UInt32?
    let applicationFocusedWindowBefore: OwnedDocumentAXRead<MenuRetainedToken>
    let applicationFocusedWindowAfter: OwnedDocumentAXRead<MenuRetainedToken>
    let systemFocusedReceiverBefore: OwnedDocumentAXRead<MenuRetainedToken>
    let systemFocusedReceiverAfter: OwnedDocumentAXRead<MenuRetainedToken>
    let applicationFocusedReceiverBefore: OwnedDocumentAXRead<MenuRetainedToken>
    let applicationFocusedReceiverAfter: OwnedDocumentAXRead<MenuRetainedToken>
    let systemFocusedReceiverObjectRef: String?
    let applicationFocusedReceiverObjectRef: String?
    let actualWindows: OwnedDocumentAXElementArrayRead
    let window: OwnedDocumentReceiverWindowRead?
    let receiver: OwnedDocumentReceiverElementRead?
    let ancestorRelations: [OwnedDocumentAncestorRelationRead]
    let ancestorChainComplete: Bool
    let ancestorChainTruncated: Bool
    let processBefore: ProcessStamp?
    let processAfter: ProcessStamp?
    let observedExecutablePath: String?
    let observedBundleID: String?
    let markedRangeBefore: OwnedDocumentMarkedRangeRead
    let markedRangeAfter: OwnedDocumentMarkedRangeRead
    let observer: OwnedDocumentObserverScopeRead
}

struct OwnedDocumentReceiverAncestryNode: Encodable, Sendable, Equatable {
    let object_ref: String
    let role: String
}

struct OwnedDocumentMarkedRangeEvidence: Encodable, Sendable {
    let receiver_object_ref: String
    let receiver_same_cf_as_actual_object_ref: Bool
    let observed_ms: UInt64
    let observation: OwnedDocumentProperty<OwnedDocumentTextRange>
}

struct OwnedDocumentPathReadEvidence: Encodable, Sendable {
    let status: OwnedDocumentAttributeStatus
    let api_code: Int32?
    let cf_type_id: UInt64?
    let observed_ms: UInt64
    let timely: Bool
    let registered_path_matches: Bool
}

struct OwnedDocumentPathBindingEvidence: Encodable, Sendable {
    let before: OwnedDocumentPathReadEvidence
    let after: OwnedDocumentPathReadEvidence
}

enum OwnedDocumentReceiverEligibilityRejection: String, Encodable, Sendable {
    case registrationInvalid = "registration_invalid"
    case sourceProvenanceInvalid = "source_provenance_invalid"
    case sourceReadIncomplete = "source_read_incomplete"
    case documentBindingMismatch = "document_binding_mismatch"
    case processIdentityMismatch = "process_identity_mismatch"
    case applicationFocusMismatch = "application_focus_mismatch"
    case windowUnknownOrAmbiguous = "window_unknown_or_ambiguous"
    case windowNotNormalOrStable = "window_not_normal_or_stable"
    case receiverFocusMismatch = "receiver_focus_mismatch"
    case receiverNotFocused = "receiver_not_focused"
    case receiverIdentityMissing = "receiver_identity_missing"
    case ancestorChainIncomplete = "ancestor_chain_incomplete"
    case observerCoverageIncomplete = "observer_coverage_incomplete"
    case observerChangedDuringRead = "observer_changed_during_read"
    case receiverRoleUnknownOrWrong = "receiver_role_unknown_or_wrong"
    case secureReceiver = "secure_receiver"
    case subroleRejected = "subrole_rejected"
    case disabledOrUnknown = "disabled_or_unknown"
    case valueNotSettable = "value_not_settable"
    case markedRangeUnknownOrChanged = "marked_range_unknown_or_changed"
    case requiredReadLate = "required_read_late"
}

struct OwnedDocumentReceiverEvidence: Encodable, Sendable {
    let registration_id: String
    let document_binding_verified: Bool
    let document_binding: OwnedDocumentPathBindingEvidence
    let profile: OwnedDocumentReceiverProfile
    let provenance: OwnedDocumentReceiverProvenance
    let process: OwnedDocumentProcessIdentity
    let window_id: String
    let window_object_ref: String
    let window_frame: Rect
    let actual_object_ref: String
    let receiver_frame: Rect
    let application_focused_object_ref: String
    let system_focused_object_ref: String
    let ancestry: [OwnedDocumentReceiverAncestryNode]
    let scope_complete: Bool
    let observer_state: OwnedDocumentObserverState
    let secure: Bool
    let role: OwnedDocumentProperty<String>
    let subrole: OwnedDocumentProperty<String>
    let enabled: OwnedDocumentProperty<Bool>
    let focused: OwnedDocumentProperty<Bool>
    let value_settable: OwnedDocumentProperty<Bool>
    let title: OwnedDocumentProperty<String>
    let description: OwnedDocumentProperty<String>
    let marked_range_before: OwnedDocumentMarkedRangeEvidence
    let marked_range_after: OwnedDocumentMarkedRangeEvidence
    let marked_range_stable_unmarked: Bool
    let profile_eligible: Bool
    var value_readback: OwnedDocumentValueReadback? = nil
}

struct OwnedDocumentReturnedProof {
    let evidence: OwnedDocumentReceiverEvidence
    let identityBoundary: SourceIdentityBoundary?
}

func ownedDocumentProofForReturn(
    evidence: OwnedDocumentReceiverEvidence?,
    actualRegistryBoundary: SourceIdentityBoundary?,
    finalProvenance: OwnedDocumentReceiverProvenance?
) -> OwnedDocumentReturnedProof? {
    guard let evidence, evidence.scope_complete,
          evidence.observer_state == .continuous,
          let expectedBoundary = evidence.provenance.identityBoundary,
          let actualRegistryBoundary, actualRegistryBoundary == expectedBoundary,
          finalProvenance == evidence.provenance else { return nil }
    return OwnedDocumentReturnedProof(evidence: evidence,
        identityBoundary: evidence.profile_eligible ? actualRegistryBoundary : nil)
}

func ownedTextSubroleIsKnownPlain(_ read: OwnedDocumentAXRead<String>,
                                  isCurrentReceiver: Bool) -> Bool {
    guard read.timely else { return false }
    if read.observation.status == .value { return read.value == "" }
    return isCurrentReceiver && read.observation.status == .unsupported
        && read.observation.code == AXError.attributeUnsupported.rawValue
}

private let ownedTextEditBundleID = "com.apple.TextEdit"
private let ownedTextEditExecutablePath = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
private let ownedDocumentMaximumAncestorDepth = 8

private func validOwnedDocumentRef(_ value: String) -> Bool {
    !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && value.utf8.count <= 256
        && !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
}

private func validOwnedDocumentRect(_ value: Rect) -> Bool {
    value.valid && value.width > 0 && value.height > 0
        && [value.x, value.y, value.width, value.height].allSatisfy { abs($0) <= 100_000 }
}

private func validOwnedDocumentRegistration(_ registration: OwnedDocumentReceiverRegistration) -> Bool {
    let path = registration.documentCanonicalPath
    // The trusted bootstrap verifies POSIX realpath. Foundation can rewrite
    // /private/var to /var, so this pure gate checks syntax without aliasing it.
    let components = path.split(separator: "/", omittingEmptySubsequences: false)
    let normalizedAbsolutePath = path.hasPrefix("/") && path.utf8.count <= 4096
        && components.count > 1 && components.dropFirst().allSatisfy {
            !$0.isEmpty && $0 != "." && $0 != ".."
        }
        && !path.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    let digest = registration.documentSHA256
    return UUID(uuidString: registration.registrationID) != nil
        && registration.pid > 0 && registration.pid <= UInt32(Int32.max)
        && registration.process.seconds > 0
        && registration.process.micros < 1_000_000
        && registration.executablePath == ownedTextEditExecutablePath
        && registration.bundleID == ownedTextEditBundleID
        && normalizedAbsolutePath
        && URL(fileURLWithPath: path).pathExtension.lowercased() == "txt"
        && digest.utf8.count == 64
        && digest.allSatisfy { $0.isHexDigit }
        && registration.privateTemporaryDocument
        && registration.plainTextDocument
        && registration.targetWasNotPreexisting
        && registration.processStartedWithinLaunchInterval
}

private func validOwnedDocumentProvenance(_ provenance: OwnedDocumentReceiverProvenance,
                                          nowMS: UInt64) -> Bool {
    guard let boundary = provenance.identityBoundary,
          UUID(uuidString: boundary.namespace) != nil,
          boundary.namespace.utf8.count == 36 && boundary.namespace == boundary.namespace.lowercased(),
          boundary.generation > 0,
          let snapshotID = provenance.snapshotID,
          validOwnedDocumentRef(snapshotID),
          provenance.sourceSequence > 0,
          provenance.observedMS > 0,
          provenance.observedMS <= nowMS,
          provenance.validUntilMS > nowMS,
          validOwnedDocumentRef(provenance.clockDomain),
          provenance.clockDomain.utf8.count <= 128 else { return false }
    return true
}

private func typedValue<Value>(_ read: OwnedDocumentAXRead<Value>, equals expected: Value,
                               isEqual: (Value, Value) -> Bool) -> Bool {
    read.observation.status == .value && read.timely
        && read.value.map { isEqual($0, expected) } == true
}

private func readIsTimely<Value>(_ read: OwnedDocumentAXRead<Value>) -> Bool {
    read.observation.status != .notAttempted && read.timely
}

private func sameToken(_ read: OwnedDocumentAXRead<MenuRetainedToken>, _ token: MenuRetainedToken) -> Bool {
    read.observation.status == .value && read.timely && read.value?.matches(token.element) == true
}

private func validAncestorChain(_ facts: OwnedDocumentReceiverReadFacts,
                                window: OwnedDocumentReceiverWindowRead,
                                receiver: OwnedDocumentReceiverElementRead,
                                registration: OwnedDocumentReceiverRegistration) -> Bool {
    let links = facts.ancestorRelations
    guard facts.ancestorChainComplete, !facts.ancestorChainTruncated,
          !links.isEmpty, links.count <= ownedDocumentMaximumAncestorDepth else { return false }
    var expectedChild = receiver.element
    var visited = [receiver.element]
    var objectRefs = Set<String>()
    for (index, link) in links.enumerated() {
        guard link.child.matches(expectedChild.element),
              sameToken(link.parentAttribute, link.parent),
              link.parentChildren.complete,
              link.parentChildren.membershipCount(of: link.child.element) == 1,
              link.parentPID == registration.pid,
              validOwnedDocumentRef(link.parentObjectRef),
              objectRefs.insert(link.parentObjectRef).inserted,
              link.parentRole.observation.status == .value,
              link.parentRole.timely,
              let role = link.parentRole.value, validOwnedDocumentRef(role), role.utf8.count <= 128,
              role != "AXSecureTextField",
              !visited.contains(where: { $0.matches(link.parent.element) }) else { return false }
        if index == links.count - 1 {
            guard link.parent.matches(window.element.element),
                  role == kAXWindowRole,
                  link.parentObjectRef == window.objectRef else { return false }
        }
        visited.append(link.parent)
        expectedChild = link.parent
    }
    return true
}

private func ancestryEvidence(_ facts: OwnedDocumentReceiverReadFacts,
                              window: OwnedDocumentReceiverWindowRead,
                              receiver: OwnedDocumentReceiverElementRead)
    -> [OwnedDocumentReceiverAncestryNode]? {
    let links = facts.ancestorRelations
    guard facts.ancestorChainComplete, !facts.ancestorChainTruncated,
          !links.isEmpty, links.count <= 8 else { return nil }
    var expectedChild = receiver.element
    var visited = [receiver.element]
    var objectRefs = Set<String>()
    var result: [OwnedDocumentReceiverAncestryNode] = []
    for (index, link) in links.enumerated() {
        guard link.child.matches(expectedChild.element),
              validOwnedDocumentRef(link.parentObjectRef),
              objectRefs.insert(link.parentObjectRef).inserted,
              link.parentRole.observation.status == .value,
              link.parentRole.timely,
              let role = link.parentRole.value, validOwnedDocumentRef(role), role.utf8.count <= 128,
              !visited.contains(where: { $0.matches(link.parent.element) }) else { return nil }
        if index == links.count - 1 {
            guard link.parent.matches(window.element.element),
                  role == kAXWindowRole,
                  link.parentObjectRef == window.objectRef else { return nil }
        }
        result.append(OwnedDocumentReceiverAncestryNode(object_ref: link.parentObjectRef, role: role))
        visited.append(link.parent)
        expectedChild = link.parent
    }
    return result
}

private func cfEqualCount(_ target: MenuRetainedToken, in values: [MenuRetainedToken]) -> Int {
    values.reduce(into: 0) { count, item in
        if item.matches(target.element) { count += 1 }
    }
}

private func identifiedNativeWindow(_ window: OwnedDocumentReceiverWindowRead) -> NativeWindow? {
    guard window.frameBefore.timely, window.frameAfter.timely,
          let frameBefore = window.frameBefore.value,
          let frameAfter = window.frameAfter.value,
          frameBefore == frameAfter,
          frameAfter.valid, frameAfter.width > 0, frameAfter.height > 0,
          let before = matchAXWindow(frameBefore, windows: window.cgWindowsBefore),
          let after = matchAXWindow(frameAfter, windows: window.cgWindowsAfter),
          before.id == after.id, before.pid == after.pid, before.frame == after.frame else { return nil }
    return before
}

private func stableNormalWindow(_ window: OwnedDocumentReceiverWindowRead) -> NativeWindow? {
    guard typedValue(window.role, equals: kAXWindowRole, isEqual: ==),
          typedValue(window.subrole, equals: kAXStandardWindowSubrole, isEqual: ==),
          typedValue(window.modal, equals: false, isEqual: ==) else { return nil }
    return identifiedNativeWindow(window)
}

private func observerAncestorTargets(_ facts: OwnedDocumentReceiverReadFacts,
                                     window: OwnedDocumentReceiverWindowRead) -> [MenuRetainedToken] {
    guard !facts.ancestorRelations.isEmpty else { return [] }
    return facts.ancestorRelations.dropLast().map(\.parent)
}

private func observerState(_ facts: OwnedDocumentReceiverReadFacts,
                           window: OwnedDocumentReceiverWindowRead,
                           receiver: OwnedDocumentReceiverElementRead) -> OwnedDocumentObserverState {
    facts.observer.state(application: facts.application, window: window.element,
        receiver: receiver.element, ancestors: observerAncestorTargets(facts, window: window))
}

private func receiverIdentityStable(_ facts: OwnedDocumentReceiverReadFacts,
                                    receiver: OwnedDocumentReceiverElementRead,
                                    window: OwnedDocumentReceiverWindowRead) -> Bool {
    let systemBefore = facts.systemFocusedReceiverBefore
    let systemAfter = facts.systemFocusedReceiverAfter
    let appBefore = facts.applicationFocusedReceiverBefore
    let appAfter = facts.applicationFocusedReceiverAfter
    let appWindowBefore = facts.applicationFocusedWindowBefore
    let appWindowAfter = facts.applicationFocusedWindowAfter
    return [systemBefore, systemAfter, appBefore, appAfter].allSatisfy {
            sameToken($0, receiver.element)
        }
        && sameToken(appWindowBefore, window.element)
        && sameToken(appWindowAfter, window.element)
        && [systemBefore, systemAfter, appBefore, appAfter, appWindowBefore, appWindowAfter]
            .allSatisfy(\.timely)
        && facts.systemFocusedReceiverObjectRef == receiver.objectRef
        && facts.applicationFocusedReceiverObjectRef == receiver.objectRef
        && !receiver.objectRef.isEmpty
}

private func markedRangeEvidence(_ read: OwnedDocumentMarkedRangeRead,
                                 receiver: OwnedDocumentReceiverElementRead) -> OwnedDocumentMarkedRangeEvidence {
    OwnedDocumentMarkedRangeEvidence(
        receiver_object_ref: receiver.objectRef,
        receiver_same_cf_as_actual_object_ref: read.receiver.matches(receiver.element.element),
        observed_ms: read.observedMS,
        observation: ownedDocumentProperty(read.read))
}

private func markedRangeEligible(_ facts: OwnedDocumentReceiverReadFacts,
                                 receiver: OwnedDocumentReceiverElementRead) -> Bool {
    let before = facts.markedRangeBefore
    let after = facts.markedRangeAfter
    guard before.receiver.matches(receiver.element.element),
          after.receiver.matches(receiver.element.element),
          before.observedMS >= facts.provenance.observedMS,
          before.observedMS <= after.observedMS,
          after.observedMS < facts.provenance.validUntilMS,
          after.observedMS <= facts.nowMS,
          before.read.timely, after.read.timely,
          before.read.observation.status == .value,
          after.read.observation.status == .value,
          let beforeRange = before.read.value,
          let afterRange = after.read.value,
          beforeRange.valid, afterRange.valid,
          beforeRange.length == 0, afterRange.length == 0,
          beforeRange == afterRange else { return false }
    return true
}

private func secureReceiver(_ receiver: OwnedDocumentReceiverElementRead) -> Bool? {
    guard receiver.role.observation.status == .value, receiver.role.timely,
          let role = receiver.role.value else { return nil }
    if role == "AXSecureTextField" { return true }
    if receiver.subrole.observation.status == .value, receiver.subrole.timely,
       receiver.subrole.value == kAXSecureTextFieldSubrole { return true }
    if role == kAXTextAreaRole { return false }
    if receiver.subrole.observation.status == .value, receiver.subrole.timely { return false }
    return nil
}

private func subroleAllowed(_ read: OwnedDocumentAXRead<String>) -> Bool {
    if read.observation.status == .value { return read.timely && read.value == "" }
    return read.observation.status == .unsupported
        && read.observation.code == AXError.attributeUnsupported.rawValue
        && read.timely
}

private func enabledAllowed(_ read: OwnedDocumentAXRead<Bool>) -> Bool {
    if read.observation.status == .value { return read.timely && read.value == true }
    return read.observation.status == .unsupported
        && read.observation.code == AXError.attributeUnsupported.rawValue
        && read.timely
}

func projectOwnedDocumentReceiver(_ facts: OwnedDocumentReceiverReadFacts)
    -> OwnedDocumentReceiverEvidence? {
    let registration = facts.registration
    let provenance = facts.provenance
    let window = facts.window
    let receiver = facts.receiver
    let observer: OwnedDocumentObserverState
    if let window, let receiver { observer = observerState(facts, window: window, receiver: receiver) }
    else { observer = .coverageIncomplete }

    let registrationValid = validOwnedDocumentRegistration(registration)
    let provenanceValid = validOwnedDocumentProvenance(provenance, nowMS: facts.nowMS)
    let processValid = facts.processBefore == registration.process
        && facts.processAfter == registration.process
        && facts.applicationPID == registration.pid
        && facts.systemFocusedApplicationPID == registration.pid
        && facts.observedBundleID == registration.bundleID
        && facts.observedExecutablePath == registration.executablePath
    let applicationFocusValid = facts.systemFocusedApplication.observation.status == .value
        && facts.systemFocusedApplication.timely
        && facts.systemFocusedApplication.value?.matches(facts.application.element) == true
    let windowReadComplete: Bool
    let windowProfileValid: Bool
    if let window {
        let identified = identifiedNativeWindow(window)
        windowReadComplete = facts.actualWindows.complete
            && cfEqualCount(window.element, in: facts.actualWindows.elements) == 1
            && identified?.pid == registration.pid && !window.objectRef.isEmpty
            && readIsTimely(window.role) && readIsTimely(window.subrole) && readIsTimely(window.modal)
        windowProfileValid = windowReadComplete && facts.actualWindows.elements.count == 1
            && stableNormalWindow(window) != nil
    } else {
        windowReadComplete = false
        windowProfileValid = false
    }
    let receiverFocusValid: Bool
    let chainValid: Bool
    if let window, let receiver {
        receiverFocusValid = receiverIdentityStable(facts, receiver: receiver, window: window)
            && receiver.pid == registration.pid
        chainValid = validAncestorChain(facts, window: window, receiver: receiver, registration: registration)
    } else {
        receiverFocusValid = false
        chainValid = false
    }
    let receiverFramesValid: Bool
    if let window, let receiver,
       let windowFrame = window.frameAfter.value,
       let receiverFrameBefore = receiver.frameBefore.value,
       let receiverFrameAfter = receiver.frameAfter.value {
        receiverFramesValid = window.frameBefore.timely && window.frameAfter.timely
            && receiver.frameBefore.timely && receiver.frameAfter.timely
            && receiverFrameBefore == receiverFrameAfter
            && receiverFrameAfter.valid && receiverFrameAfter.width > 0 && receiverFrameAfter.height > 0
            && windowFrame.cgRect.contains(receiverFrameAfter.cgRect)
    } else {
        receiverFramesValid = false
    }
    let receiverReadsComplete: Bool
    let rangeReadsComplete = readIsTimely(facts.markedRangeBefore.read)
        && readIsTimely(facts.markedRangeAfter.read)
        && facts.markedRangeBefore.observedMS >= provenance.observedMS
        && facts.markedRangeBefore.observedMS <= facts.markedRangeAfter.observedMS
        && facts.markedRangeAfter.observedMS < provenance.validUntilMS
        && facts.markedRangeAfter.observedMS <= facts.nowMS
    if let receiver {
        receiverReadsComplete = readIsTimely(receiver.role) && readIsTimely(receiver.subrole)
            && readIsTimely(receiver.enabled) && readIsTimely(receiver.focused)
            && readIsTimely(receiver.valueSettable) && rangeReadsComplete
    } else {
        receiverReadsComplete = false
    }
    let scopeComplete = registrationValid && provenanceValid && facts.ownedScopeReadComplete
        && facts.documentBindingVerified
        && facts.documentBinding.before.registered_path_matches
        && facts.documentBinding.after.registered_path_matches
        && processValid && applicationFocusValid && windowReadComplete && receiverFocusValid
        && chainValid && receiverFramesValid && receiverReadsComplete && observer == .continuous

    var rejection: OwnedDocumentReceiverEligibilityRejection?
    if !scopeComplete {
        if !registrationValid { rejection = .registrationInvalid }
        else if !provenanceValid { rejection = .sourceProvenanceInvalid }
        else if !facts.ownedScopeReadComplete { rejection = .sourceReadIncomplete }
        else if !facts.documentBindingVerified { rejection = .documentBindingMismatch }
        else if !processValid { rejection = .processIdentityMismatch }
        else if !applicationFocusValid { rejection = .applicationFocusMismatch }
        else if !windowProfileValid || !facts.noModalOrTransient { rejection = .windowUnknownOrAmbiguous }
        else if !receiverFocusValid { rejection = .receiverFocusMismatch }
        else if !chainValid { rejection = .ancestorChainIncomplete }
        else if observer == .changedDuringRead { rejection = .observerChangedDuringRead }
        else if observer != .continuous { rejection = .observerCoverageIncomplete }
        else { rejection = .windowNotNormalOrStable }
    } else if !windowProfileValid || !facts.noModalOrTransient {
        rejection = .windowNotNormalOrStable
    } else if let receiver {
        let secure = secureReceiver(receiver)
        if receiver.role.observation.status != .value || !receiver.role.timely
            || receiver.role.value != kAXTextAreaRole {
            rejection = .receiverRoleUnknownOrWrong
        } else if secure != false {
            rejection = .secureReceiver
        } else if !subroleAllowed(receiver.subrole) {
            rejection = .subroleRejected
        } else if !enabledAllowed(receiver.enabled) {
            rejection = .disabledOrUnknown
        } else if !typedValue(receiver.focused, equals: true, isEqual: ==) {
            rejection = .receiverNotFocused
        } else if !typedValue(receiver.valueSettable, equals: true, isEqual: ==) {
            rejection = .valueNotSettable
        } else if !markedRangeEligible(facts, receiver: receiver) {
            rejection = .markedRangeUnknownOrChanged
        }
    } else {
        rejection = .receiverIdentityMissing
    }
    if rejection == nil, let receiver,
       !receiver.frameBefore.timely || !receiver.frameAfter.timely {
        rejection = .requiredReadLate
    }

    guard registrationValid, provenanceValid,
          let processStamp = facts.processAfter,
          let observedPID = facts.applicationPID, observedPID > 0,
          observedPID <= UInt32(Int32.max), processStamp.seconds > 0,
          processStamp.micros < 1_000_000,
          let executablePath = facts.observedExecutablePath, !executablePath.isEmpty,
          let bundleID = facts.observedBundleID, !bundleID.isEmpty,
          let window, let receiver,
          let identifiedWindow = identifiedNativeWindow(window),
          validOwnedDocumentRef(identifiedWindow.ref),
          validOwnedDocumentRef(window.objectRef), validOwnedDocumentRef(receiver.objectRef),
          let windowFrame = window.frameAfter.value,
          let receiverFrame = receiver.frameAfter.value,
          validOwnedDocumentRect(windowFrame), validOwnedDocumentRect(receiverFrame),
          let applicationFocusedObjectRef = facts.applicationFocusedReceiverObjectRef,
          validOwnedDocumentRef(applicationFocusedObjectRef),
          let systemFocusedObjectRef = facts.systemFocusedReceiverObjectRef,
          validOwnedDocumentRef(systemFocusedObjectRef),
          let ancestry = ancestryEvidence(facts, window: window, receiver: receiver),
          let secure = secureReceiver(receiver) else { return nil }
    let receiverProcess = OwnedDocumentProcessIdentity(pid: observedPID,
        startSeconds: processStamp.seconds, startMicros: processStamp.micros)
    let rangeStable = markedRangeEligible(facts, receiver: receiver)
    return OwnedDocumentReceiverEvidence(
        registration_id: registration.registrationID,
        document_binding_verified: facts.documentBindingVerified,
        document_binding: facts.documentBinding,
        profile: .ownedPlainTextValueSetterV1,
        provenance: provenance,
        process: receiverProcess,
        window_id: identifiedWindow.ref,
        window_object_ref: window.objectRef,
        window_frame: windowFrame,
        actual_object_ref: receiver.objectRef,
        receiver_frame: receiverFrame,
        application_focused_object_ref: applicationFocusedObjectRef,
        system_focused_object_ref: systemFocusedObjectRef,
        ancestry: ancestry,
        scope_complete: scopeComplete,
        observer_state: observer,
        secure: secure,
        role: ownedDocumentProperty(receiver.role),
        subrole: ownedDocumentProperty(receiver.subrole),
        enabled: ownedDocumentProperty(receiver.enabled),
        focused: ownedDocumentProperty(receiver.focused),
        value_settable: ownedDocumentProperty(receiver.valueSettable),
        title: ownedDocumentProperty(receiver.title),
        description: ownedDocumentProperty(receiver.description),
        marked_range_before: markedRangeEvidence(facts.markedRangeBefore, receiver: receiver),
        marked_range_after: markedRangeEvidence(facts.markedRangeAfter, receiver: receiver),
        marked_range_stable_unmarked: rangeStable,
        profile_eligible: scopeComplete && windowProfileValid && facts.noModalOrTransient && rejection == nil)
}
