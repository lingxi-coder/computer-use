import Foundation
import ApplicationServices
import CoreGraphics

private func ownedRead<Value>(_ value: Value, timely: Bool = true) -> OwnedDocumentAXRead<Value> {
    OwnedDocumentAXRead(value: value,
        observation: OwnedDocumentAttributeObservation(status: .value, code: nil,
            actualCFTypeID: nil, reason: nil),
        cfTypeID: nil, returnedLate: !timely, timely: timely)
}

private func ownedStringRead(_ value: String, timely: Bool = true) -> OwnedDocumentAXRead<String> {
    OwnedDocumentAXRead(value: value,
        observation: OwnedDocumentAttributeObservation(status: .value, code: nil,
            actualCFTypeID: UInt64(CFStringGetTypeID()), reason: nil),
        cfTypeID: CFStringGetTypeID(), returnedLate: !timely, timely: timely)
}

private func ownedStatusRead<Value>(_ status: OwnedDocumentAttributeStatus,
                                    code: Int32? = nil,
                                    reason: OwnedDocumentNotAttemptedReason? = nil,
                                    timely: Bool = true) -> OwnedDocumentAXRead<Value> {
    OwnedDocumentAXRead(value: nil,
        observation: OwnedDocumentAttributeObservation(status: status, code: code,
            actualCFTypeID: nil, reason: reason),
        cfTypeID: nil, returnedLate: !timely, timely: timely)
}

private func ownedArray(_ elements: [MenuRetainedToken], rawCount: Int? = nil,
                        truncated: Bool = false) -> OwnedDocumentAXElementArrayRead {
    let count = elements.count
    return OwnedDocumentAXElementArrayRead(
        observation: OwnedDocumentAttributeObservation(status: .value, code: nil,
            actualCFTypeID: nil, reason: nil),
        elements: elements, rawElementCount: rawCount ?? count,
        returnedElementCount: count, truncated: truncated, conversionSucceeded: true, returnedLate: false)
}

private func ownedPathEvidence(matches: Bool) -> OwnedDocumentPathBindingEvidence {
    let pathRead = OwnedDocumentPathReadEvidence(status: .value, api_code: 0,
        cf_type_id: UInt64(CFURLGetTypeID()), observed_ms: 220, timely: true,
        registered_path_matches: matches)
    return OwnedDocumentPathBindingEvidence(before: pathRead, after: pathRead)
}

private func ownedObserverRegistrations(app: MenuRetainedToken, window: MenuRetainedToken,
                                        receiver: MenuRetainedToken,
                                        ancestors: [MenuRetainedToken] = []) -> [OwnedDocumentObserverRegistrationRead] {
    let rows: [(MenuRetainedToken, OwnedDocumentObserverNotification)] = [
        (app, .focusedWindowChanged), (app, .focusedElementChanged), (app, .windowCreated),
        (app, .applicationActivated), (app, .applicationDeactivated),
        (window, .elementDestroyed), (window, .layoutChanged), (window, .windowMoved), (window, .windowResized),
        (receiver, .elementDestroyed)
    ] + ancestors.map { ($0, .elementDestroyed) }
    return rows.map { element, notification in
        OwnedDocumentObserverRegistrationRead(element: element, notification: notification,
            attempted: true, errorCode: AXError.success.rawValue, returnedLate: false)
    }
}

private func ownedReceiverFacts(
    enabled: OwnedDocumentAXRead<Bool>? = nil,
    subrole: OwnedDocumentAXRead<String>? = nil,
    valueSettable: OwnedDocumentAXRead<Bool>? = nil,
    role: OwnedDocumentAXRead<String>? = nil,
    focused: OwnedDocumentAXRead<Bool>? = nil,
    title: OwnedDocumentAXRead<String>? = nil,
    receiverToken: MenuRetainedToken? = nil,
    actualWindows: OwnedDocumentAXElementArrayRead? = nil,
    parentChildren: OwnedDocumentAXElementArrayRead? = nil,
    markedBefore: OwnedDocumentMarkedRangeRead? = nil,
    markedAfter: OwnedDocumentMarkedRangeRead? = nil,
    observer: OwnedDocumentObserverScopeRead? = nil,
    missingSystemFocusedObjectRef: Bool = false,
    documentBindingVerified: Bool = true,
    documentPathMatches: Bool = true,
    documentCanonicalPath: String = "/tmp/owned-document.txt",
    secureIntermediateAncestor: Bool = false,
    ownedScopeReadComplete: Bool = true,
    identityBoundary: SourceIdentityBoundary = SourceIdentityBoundary(
        namespace: "00000000-0000-4000-8000-000000000002", generation: 3),
    nowMS: UInt64 = 250
) -> OwnedDocumentReceiverReadFacts {
    let app = MenuRetainedToken("owned-test-app" as CFString)
    let window = MenuRetainedToken("owned-test-window" as CFString)
    let receiver = receiverToken ?? MenuRetainedToken("owned-test-receiver" as CFString)
    let stamp = ProcessStamp(seconds: 77, micros: 12)
    let registration = OwnedDocumentReceiverRegistration(
        registrationID: "00000000-0000-4000-8000-000000000001", pid: 1234, process: stamp,
        executablePath: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        bundleID: "com.apple.TextEdit", documentCanonicalPath: documentCanonicalPath,
        documentSHA256: String(repeating: "a", count: 64), privateTemporaryDocument: true,
        plainTextDocument: true, targetWasNotPreexisting: true, processStartedWithinLaunchInterval: true)
    let provenance = OwnedDocumentReceiverProvenance(
        identityBoundary: identityBoundary,
        snapshotID: "snapshot-3", sourceSequence: 8, observedMS: 200, validUntilMS: 400,
        clockDomain: "continuous")
    let rect = Rect(CGRect(x: 40, y: 50, width: 500, height: 300))
    let nativeWindow = NativeWindow(id: 54, pid: 1234, frame: rect, zIndex: 0)
    let receiverRead = OwnedDocumentReceiverElementRead(
        element: receiver, objectRef: "receiver-ref", pid: 1234,
        frameBefore: ownedRead(rect), frameAfter: ownedRead(rect),
        role: role ?? ownedRead(kAXTextAreaRole), subrole: subrole ?? ownedRead(""),
        enabled: enabled ?? ownedRead(true), focused: focused ?? ownedRead(true),
        valueSettable: valueSettable ?? ownedRead(true), title: title ?? ownedRead("Owned document"),
        description: ownedStatusRead(.unsupported, code: AXError.attributeUnsupported.rawValue))
    let windowRead = OwnedDocumentReceiverWindowRead(
        element: window, objectRef: "window-ref", role: ownedRead(kAXWindowRole),
        subrole: ownedRead(kAXStandardWindowSubrole), modal: ownedRead(false),
        frameBefore: ownedRead(rect), frameAfter: ownedRead(rect),
        cgWindowsBefore: [nativeWindow], cgWindowsAfter: [nativeWindow])
    let links: [OwnedDocumentAncestorRelationRead]
    let observerAncestors: [MenuRetainedToken]
    if secureIntermediateAncestor {
        let secureParent = MenuRetainedToken("owned-test-secure-parent" as CFString)
        links = [
            OwnedDocumentAncestorRelationRead(child: receiver, parent: secureParent,
                parentAttribute: ownedRead(secureParent), parentChildren: ownedArray([receiver]),
                parentPID: 1234, parentObjectRef: "secure-parent-ref", parentRole: ownedRead("AXSecureTextField")),
            OwnedDocumentAncestorRelationRead(child: secureParent, parent: window,
                parentAttribute: ownedRead(window), parentChildren: ownedArray([secureParent]),
                parentPID: 1234, parentObjectRef: "window-ref", parentRole: ownedRead(kAXWindowRole))
        ]
        observerAncestors = [secureParent]
    } else {
        links = [OwnedDocumentAncestorRelationRead(
            child: receiver, parent: window, parentAttribute: ownedRead(window),
            parentChildren: parentChildren ?? ownedArray([receiver]), parentPID: 1234,
            parentObjectRef: "window-ref", parentRole: ownedRead(kAXWindowRole))]
        observerAncestors = []
    }
    let defaultObserver = OwnedDocumentObserverScopeRead(
        sameWatch: true, observerAvailableBefore: true, observerAvailableAfter: true,
        generationBefore: 3, generationAfter: 3, revisionBefore: 11, revisionAfter: 11,
        registrations: ownedObserverRegistrations(app: app, window: window, receiver: receiver,
            ancestors: observerAncestors))
    let zero = OwnedDocumentTextRange(location: 5, length: 0)
    let before = markedBefore ?? OwnedDocumentMarkedRangeRead(receiver: receiver, observedMS: 210, read: ownedRead(zero))
    let after = markedAfter ?? OwnedDocumentMarkedRangeRead(receiver: receiver, observedMS: 220, read: ownedRead(zero))
    return OwnedDocumentReceiverReadFacts(
        registration: registration, documentBindingVerified: documentBindingVerified,
        documentBinding: ownedPathEvidence(matches: documentPathMatches),
        provenance: provenance, nowMS: nowMS,
        ownedScopeReadComplete: ownedScopeReadComplete, noModalOrTransient: true, application: app, applicationPID: 1234,
        systemFocusedApplication: ownedRead(app), systemFocusedApplicationPID: 1234,
        applicationFocusedWindowBefore: ownedRead(window), applicationFocusedWindowAfter: ownedRead(window),
        systemFocusedReceiverBefore: ownedRead(receiver), systemFocusedReceiverAfter: ownedRead(receiver),
        applicationFocusedReceiverBefore: ownedRead(receiver), applicationFocusedReceiverAfter: ownedRead(receiver),
        systemFocusedReceiverObjectRef: missingSystemFocusedObjectRef ? nil : "receiver-ref",
        applicationFocusedReceiverObjectRef: "receiver-ref",
        actualWindows: actualWindows ?? ownedArray([window]), window: windowRead, receiver: receiverRead,
        ancestorRelations: links, ancestorChainComplete: true, ancestorChainTruncated: false,
        processBefore: stamp, processAfter: stamp,
        observedExecutablePath: registration.executablePath, observedBundleID: registration.bundleID,
        markedRangeBefore: before, markedRangeAfter: after, observer: observer ?? defaultObserver)
}

func runOwnedDocumentReceiverTests() throws -> [String] {
    var passed: [String] = []
    let fileURL = URL(fileURLWithPath: "/tmp/owned-document-url-test.txt")
    let cfURLRead = projectOwnedDocumentURLAttribute(errorCode: AXError.success.rawValue,
        rawValue: fileURL as CFURL, attempted: true, returnedLate: false)
    let cfStringRead = projectOwnedDocumentURLAttribute(errorCode: AXError.success.rawValue,
        rawValue: fileURL.absoluteString as CFString, attempted: true, returnedLate: false)
    let wrongTypeRead = projectOwnedDocumentURLAttribute(errorCode: AXError.success.rawValue,
        rawValue: NSNumber(value: 1), attempted: true, returnedLate: false)
    let unsupportedDocumentRead = projectOwnedDocumentURLAttribute(
        errorCode: AXError.attributeUnsupported.rawValue, rawValue: nil,
        attempted: true, returnedLate: false)
    try requireTest(cfURLRead.observation.status == .value && cfURLRead.value?.isFileURL == true
        && cfStringRead.observation.status == .value && cfStringRead.value?.isFileURL == true
        && wrongTypeRead.observation.status == .decodeFailure
        && wrongTypeRead.observation.code == AXError.success.rawValue
        && wrongTypeRead.observation.actualCFTypeID == UInt64(CFGetTypeID(NSNumber(value: 1)))
        && unsupportedDocumentRead.observation.status == .unsupported
        && unsupportedDocumentRead.observation.code == AXError.attributeUnsupported.rawValue,
        "AXDocument preserves successful CFURL/String decoding, wrong-type API success, and Unsupported status")
    passed.append("AXDocument typed URL decoder")
    try requireTest(isLocalOwnedDocumentFileURL(URL(fileURLWithPath: "/tmp/private.txt"))
        && isLocalOwnedDocumentFileURL(URL(string: "file://localhost/tmp/private.txt")!)
        && !isLocalOwnedDocumentFileURL(URL(string: "file://remote.invalid/tmp/private.txt")!)
        && !isLocalOwnedDocumentFileURL(URL(string: "file:///tmp/private.txt?query=1")!)
        && !isLocalOwnedDocumentFileURL(URL(string: "file:///tmp/private.txt#fragment")!)
        && !isLocalOwnedDocumentFileURL(URL(string: "file://user@localhost/tmp/private.txt")!),
        "AXDocument binding accepts only local file URLs without authority or suffix")
    passed.append("local AXDocument URL gate")
    let validFacts = ownedReceiverFacts()
    guard let valid = projectOwnedDocumentReceiver(validFacts) else {
        throw NativeFailure("owned receiver positive fixture lacked required identity")
    }
    let validGateSummary = ownedDocumentProjectionGateSummary(validFacts)
    try requireTest(valid.scope_complete && valid.profile_eligible,
                    "registered fresh normal TextEdit receiver passes owned plain-text profile")
    try requireTest(validGateSummary.scope_complete == valid.scope_complete
                    && validGateSummary.observer_state == valid.observer_state
                    && validGateSummary.required_identity_fields_valid
                    && validGateSummary.receiver_is_secure == false,
                    "private gate summary reports the same accepted scope and known non-secure receiver")
    passed.append("owned receiver positive proof")

    let readback = projectOwnedDocumentValueReadback(ownedStringRead("abc"),
        receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
        observedMS: 230, validUntilMS: 400)
    try requireTest(readback?.receiver_object_ref == "receiver-ref"
        && readback?.receiver_same_cf_as_actual_object_ref == true
        && readback?.observed_ms == 230
        && readback?.sha256 == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        && readback?.utf8_byte_count == 3,
        "bounded readback hashes the exact UTF-8 value without retaining text")
    let unicodeReadback = projectOwnedDocumentValueReadback(ownedStringRead("中文"),
        receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
        observedMS: 230, validUntilMS: 400)
    try requireTest(unicodeReadback?.utf8_byte_count == 6,
                    "readback byte count uses UTF-8 bytes for non-ASCII text")
    let maximumReadback = projectOwnedDocumentValueReadback(
        ownedStringRead(String(repeating: "a", count: 2_048)),
        receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
        observedMS: 230, validUntilMS: 400)
    try requireTest(maximumReadback?.utf8_byte_count == 2_048
        && projectOwnedDocumentValueReadback(ownedStringRead(String(repeating: "a", count: 2_049)),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
            observedMS: 230, validUntilMS: 400) == nil
        && projectOwnedDocumentValueReadback(ownedStringRead(String(repeating: "é", count: 1_025)),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
            observedMS: 230, validUntilMS: 400) == nil,
        "readback accepts the byte limit and omits oversized values without truncation")
    try requireTest(projectOwnedDocumentValueReadback(ownedStringRead("abc", timely: false),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
            observedMS: 230, validUntilMS: 400) == nil
        && projectOwnedDocumentValueReadback(ownedStringRead("abc"),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: false,
            observedMS: 230, validUntilMS: 400) == nil
        && projectOwnedDocumentValueReadback(ownedStringRead("abc"),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
            observedMS: 400, validUntilMS: 400) == nil
        && projectOwnedDocumentValueReadback(ownedStringRead("abc"),
            receiverObjectRef: "", receiverSameCFAsActualObjectRef: true,
            observedMS: 230, validUntilMS: 400) == nil
        && projectOwnedDocumentValueReadback(ownedStatusRead(.unsupported,
                code: AXError.attributeUnsupported.rawValue),
            receiverObjectRef: "receiver-ref", receiverSameCFAsActualObjectRef: true,
            observedMS: 230, validUntilMS: 400) == nil,
        "missing, late, uncorrelated and expired reads produce no digest")
    passed.append("bounded exact AXValue readback projection")
    try requireTest(boundedOwnedDocumentValue("" as CFString) == ""
        && boundedOwnedDocumentValue(String(repeating: "a", count: 2_048) as CFString)?.utf8.count == 2_048
        && boundedOwnedDocumentValue(String(repeating: "a", count: 2_049) as CFString) == nil
        && boundedOwnedDocumentValue(String(repeating: "é", count: 1_025) as CFString) == nil
        && boundedOwnedDocumentValue("中文\tCafe\u{301} 👩🏽‍💻\r\n" as CFString) == "中文\tCafe\u{301} 👩🏽‍💻\r\n"
        && boundedOwnedDocumentValue(kCFBooleanTrue) == nil,
        "bounded CFString decoding preserves UTF-8 and rejects wrong types or oversized values without truncation")
    passed.append("bounded AXValue CFString decoder")
    let privateVarFacts = ownedReceiverFacts(documentCanonicalPath: "/private/var/folders/owned/document.txt")
    try requireTest(projectOwnedDocumentReceiver(privateVarFacts)?.profile_eligible == true,
                    "POSIX private var registration remains valid")
    for path in ["relative/document.txt", "/private/var/../document.txt",
                 "/private//var/document.txt", "/private/var/./document.txt",
                 "/private/var/document.txt/", "/private/var/\n/document.txt"] {
        try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(documentCanonicalPath: path)) == nil,
                        "non-normalized owned registration rejected")
    }
    passed.append("owned registration preserves POSIX private var path")

    let boundaryRegistry = AXIdentityRegistry(capacity: 2,
        sessionID: valid.provenance.identityBoundary!.namespace)
    guard let actualBoundary = boundaryRegistry.ownedScopeIdentityBoundary(complete: true),
          let matchingEvidence = projectOwnedDocumentReceiver(
            ownedReceiverFacts(identityBoundary: actualBoundary)) else {
        throw NativeFailure("matching private owned identity boundary fixture unavailable")
    }
    guard let matchingProvenance = OwnedDocumentReadScope(snapshotID: "snapshot-3",
        sourceSequence: 8, observedMS: 200, validUntilMS: 400, clockDomain: "continuous")
        .provenance(identityBoundary: actualBoundary, readStartedMS: 200, readFinishedMS: 250) else {
        throw NativeFailure("matching private owned provenance fixture unavailable")
    }
    guard let returnedProof = ownedDocumentProofForReturn(evidence: matchingEvidence,
        actualRegistryBoundary: actualBoundary, finalProvenance: matchingProvenance)
    else {
        throw NativeFailure("eligible owned proof failed the final actual-registry boundary gate")
    }
    var genericStructure = Structure(app: AppIdentity(bundle_id: "offline.test", name: "Offline", pid: 1))
    genericStructure.owned_document_evidence = returnedProof.evidence
    let privateRead = AXSessionRead(read: StructureRead(structure: genericStructure,
        started: 1, finished: 2, secureFrames: []), tree_revision: 1, observer_revision: 1,
        observer_available: true, observer_reason: nil, started_unix_ms: 200, finished_unix_ms: 250,
        identity_boundary: nil, owned_document_identity_boundary: returnedProof.identityBoundary,
        owned_document_evidence: returnedProof.evidence)
    try requireTest(privateRead.owned_document_identity_boundary == returnedProof.evidence.provenance.identityBoundary
                    && privateRead.identity_boundary == nil
                    && privateRead.read.structure.identity_boundary == nil,
                    "eligible owned proof returns its actual registry boundary independently of generic completeness")
    guard let ineligibleEvidence = projectOwnedDocumentReceiver(ownedReceiverFacts(
        enabled: ownedRead(false), identityBoundary: actualBoundary)),
          let ineligibleProof = ownedDocumentProofForReturn(evidence: ineligibleEvidence,
              actualRegistryBoundary: actualBoundary, finalProvenance: ineligibleEvidence.provenance) else {
        throw NativeFailure("ineligible owned scope evidence was not retained for diagnostics")
    }
    try requireTest(!ineligibleProof.evidence.profile_eligible && ineligibleProof.identityBoundary == nil,
                    "ineligible typed evidence remains diagnostic without a private return boundary")
    boundaryRegistry.invalidate()
    let postInvalidationBoundary = boundaryRegistry.ownedScopeIdentityBoundary(complete: true)
    let staleProof = ownedDocumentProofForReturn(evidence: matchingEvidence,
        actualRegistryBoundary: postInvalidationBoundary, finalProvenance: matchingProvenance)
    var staleStructure = Structure(app: AppIdentity(bundle_id: "offline.test", name: "Offline", pid: 1))
    staleStructure.owned_document_evidence = staleProof?.evidence
    let staleRead = AXSessionRead(read: StructureRead(structure: staleStructure,
        started: 1, finished: 2, secureFrames: []), tree_revision: 2, observer_revision: 2,
        observer_available: false, observer_reason: nil, started_unix_ms: 200, finished_unix_ms: 251,
        identity_boundary: nil, owned_document_identity_boundary: staleProof?.identityBoundary,
        owned_document_evidence: staleProof?.evidence)
    try requireTest(staleRead.owned_document_identity_boundary == nil
                    && staleRead.owned_document_evidence == nil
                    && staleRead.read.structure.owned_document_evidence == nil,
                    "registry invalidation prevents an old proof or sidecar from returning")
    passed.append("owned identity boundary return gate")

    let unsupportedSubrole = ownedStatusRead(.unsupported,
        code: AXError.attributeUnsupported.rawValue) as OwnedDocumentAXRead<String>
    try requireTest(ownedTextSubroleIsKnownPlain(ownedRead(""), isCurrentReceiver: false)
                    && ownedTextSubroleIsKnownPlain(unsupportedSubrole, isCurrentReceiver: true)
                    && !ownedTextSubroleIsKnownPlain(unsupportedSubrole, isCurrentReceiver: false)
                    && !ownedTextSubroleIsKnownPlain(ownedStatusRead(.unsupported,
                        code: AXError.cannotComplete.rawValue), isCurrentReceiver: true)
                    && !ownedTextSubroleIsKnownPlain(ownedRead("AXUnknown"), isCurrentReceiver: true),
                    "unsupported subrole is a profile exception only for the actual focused receiver")
    passed.append("registered receiver subrole exception")

    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(documentBindingVerified: false))?
            .profile_eligible == false,
        "document binding mismatch cannot qualify the owned receiver")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(documentPathMatches: false))?
            .profile_eligible == false,
        "before/after document path evidence must both match the registered canonical path")

    guard let unsupported = projectOwnedDocumentReceiver(ownedReceiverFacts(
        enabled: ownedStatusRead(.unsupported, code: AXError.attributeUnsupported.rawValue),
        subrole: ownedStatusRead(.unsupported, code: AXError.attributeUnsupported.rawValue))) else {
        throw NativeFailure("unsupported-value fixture lacked required identity")
    }
    try requireTest(unsupported.profile_eligible
        && unsupported.enabled.status == .unsupported && unsupported.enabled.code == AXError.attributeUnsupported.rawValue
        && unsupported.subrole.status == .unsupported && unsupported.subrole.code == AXError.attributeUnsupported.rawValue,
        "unsupported enabled and subrole remain typed and are profile-accepted")
    passed.append("typed unsupported policy")

    for read in [ownedRead(false), ownedStatusRead(.noValue, code: AXError.noValue.rawValue),
                 ownedStatusRead(.failure, code: AXError.cannotComplete.rawValue)] as [OwnedDocumentAXRead<Bool>] {
        let evidence = projectOwnedDocumentReceiver(ownedReceiverFacts(enabled: read))
        try requireTest(evidence?.scope_complete == true && evidence?.profile_eligible == false,
                        "enabled false or unavailable is not eligible")
    }
    let notAttempted = projectOwnedDocumentReceiver(ownedReceiverFacts(
        enabled: ownedStatusRead(.notAttempted, reason: .deadline, timely: false)))
    try requireTest(notAttempted?.scope_complete == false && notAttempted?.profile_eligible == false,
                    "unread required enabled state makes the owned scope incomplete")
    for read in [ownedRead(false), ownedStatusRead(.unsupported, code: AXError.attributeUnsupported.rawValue),
                 ownedStatusRead(.noValue, code: AXError.noValue.rawValue)] as [OwnedDocumentAXRead<Bool>] {
        let evidence = projectOwnedDocumentReceiver(ownedReceiverFacts(focused: read))
        try requireTest(evidence?.scope_complete == true && evidence?.profile_eligible == false,
                        "focused must be explicitly true for the setter profile")
    }
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(
        subrole: ownedStatusRead(.noValue, code: AXError.noValue.rawValue)))?.profile_eligible != true,
        "subrole noValue is rejected")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(
        subrole: ownedRead("AXUnknown")))?.profile_eligible != true,
        "unknown subrole is rejected")
    let secure = projectOwnedDocumentReceiver(ownedReceiverFacts(
        subrole: ownedRead(kAXSecureTextFieldSubrole)))
    try requireTest(secure?.scope_complete == true && secure?.secure == true && secure?.profile_eligible == false,
                    "secure receiver remains explicitly rejected")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(
        enabled: ownedStatusRead(.unsupported, code: AXError.cannotComplete.rawValue)))?.profile_eligible != true,
        "unsupported enabled state must preserve the exact AXUnsupported code")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(valueSettable: ownedRead(false)))?.profile_eligible != true,
                    "unsettable value is rejected")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(role: ownedRead(kAXTextFieldRole)))?.profile_eligible != true,
                    "non TextEdit text area role is rejected")
    passed.append("unknown and non-settable rejection")

    let markedRange = OwnedDocumentTextRange(location: 5, length: 1)
    let composing = projectOwnedDocumentReceiver(ownedReceiverFacts(
        markedBefore: OwnedDocumentMarkedRangeRead(receiver: MenuRetainedToken("owned-test-receiver" as CFString),
            observedMS: 210, read: ownedRead(markedRange)),
        markedAfter: OwnedDocumentMarkedRangeRead(receiver: MenuRetainedToken("owned-test-receiver" as CFString),
            observedMS: 220, read: ownedRead(markedRange))))
    try requireTest(composing?.scope_complete == true && composing?.profile_eligible == false,
        "marked text composition is rejected")
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(
        markedBefore: OwnedDocumentMarkedRangeRead(receiver: MenuRetainedToken("owned-test-receiver" as CFString),
            observedMS: 190, read: ownedRead(OwnedDocumentTextRange(location: 5, length: 0)))))?.profile_eligible != true,
        "pre-scope marked range cannot be reused as fresh composition evidence")
    let wrongReceiverRange = projectOwnedDocumentReceiver(ownedReceiverFacts(
        markedBefore: OwnedDocumentMarkedRangeRead(receiver: MenuRetainedToken("other-receiver" as CFString),
            observedMS: 210, read: ownedRead(OwnedDocumentTextRange(location: 5, length: 0)))))
    try requireTest(wrongReceiverRange?.scope_complete == true && wrongReceiverRange?.profile_eligible == false,
                    "marked range tied to another CF receiver is rejected")
    let expiredRange = projectOwnedDocumentReceiver(ownedReceiverFacts(
        markedAfter: OwnedDocumentMarkedRangeRead(receiver: MenuRetainedToken("owned-test-receiver" as CFString),
            observedMS: 400, read: ownedRead(OwnedDocumentTextRange(location: 5, length: 0)))))
    try requireTest(expiredRange?.scope_complete == false && expiredRange?.profile_eligible == false,
                    "marked range at the source expiry boundary is stale")
    passed.append("fresh unmarked range gate")

    let twoWindows = ownedArray([MenuRetainedToken("owned-test-window" as CFString),
                                 MenuRetainedToken("other-window" as CFString)])
    let ambiguousWindow = projectOwnedDocumentReceiver(ownedReceiverFacts(actualWindows: twoWindows))
    try requireTest(ambiguousWindow?.scope_complete == true && ambiguousWindow?.profile_eligible == false,
                    "complete but ambiguous window inventory is not profile eligible")
    let truncatedChildren = ownedArray([MenuRetainedToken("owned-test-receiver" as CFString)], rawCount: 2,
                                       truncated: true)
    try requireTest(projectOwnedDocumentReceiver(ownedReceiverFacts(parentChildren: truncatedChildren))?.scope_complete != true,
                    "truncated ancestry inventory is incomplete")
    let secureAncestor = projectOwnedDocumentReceiver(ownedReceiverFacts(secureIntermediateAncestor: true))
    try requireTest(secureAncestor?.scope_complete == false && secureAncestor?.profile_eligible == false,
                    "secure intermediate ancestor invalidates the identity chain")
    passed.append("complete unique hierarchy gate")

    let missingIdentityFacts = ownedReceiverFacts(missingSystemFocusedObjectRef: true)
    let missingIdentitySummary = ownedDocumentProjectionGateSummary(missingIdentityFacts)
    try requireTest(projectOwnedDocumentReceiver(missingIdentityFacts) == nil
                    && !missingIdentitySummary.required_identity_fields_valid,
                    "missing required identity reference yields unavailable rather than a placeholder")
    passed.append("required identity unavailable")

    let incompleteObserver = OwnedDocumentObserverScopeRead(
        sameWatch: true, observerAvailableBefore: true, observerAvailableAfter: true,
        generationBefore: 3, generationAfter: 3, revisionBefore: 11, revisionAfter: 11,
        registrations: [])
    let incompleteObserverFacts = ownedReceiverFacts(observer: incompleteObserver)
    let incompleteObserverEvidence = projectOwnedDocumentReceiver(incompleteObserverFacts)
    let incompleteObserverSummary = ownedDocumentProjectionGateSummary(incompleteObserverFacts)
    try requireTest(incompleteObserverEvidence?.scope_complete != true
                    && incompleteObserverSummary.observer_state == .coverageIncomplete
                    && !incompleteObserverSummary.scope_complete,
                    "missing observer coverage is incomplete")
    let incompleteScopeFacts = ownedReceiverFacts(ownedScopeReadComplete: false)
    guard let observerRejected = incompleteObserverEvidence,
          let scopeRejected = projectOwnedDocumentReceiver(incompleteScopeFacts) else {
        throw NativeFailure("projection gate failure fixtures lacked typed evidence")
    }
    try requireTest(ownedDocumentProjectionGate(nil) == .evidenceUnavailable
                    && ownedDocumentProjectionGate(observerRejected) == .observerNotContinuous
                    && ownedDocumentProjectionGate(scopeRejected) == .scopeIncomplete
                    && ownedDocumentProjectionGate(valid) == .accepted,
                    "pure owned projection gate separates missing evidence, observer loss, and incomplete scope")
    let diagnostic = OwnedDocumentProjectionGateDiagnostic(sequence: 1, result: .observerNotContinuous,
        evidence: observerRejected, gateSummary: incompleteObserverSummary)
    let diagnosticData = try JSONEncoder().encode(diagnostic)
    let diagnosticText = String(decoding: diagnosticData, as: UTF8.self)
    try requireTest(diagnosticData.count <= 768
                    && !diagnosticText.contains("/tmp/")
                    && !diagnosticText.contains("owned-test"),
                    "one projection gate diagnostic stays within its byte cap and omits identities and paths")
    passed.append("observer continuity gate")

    let encoded = try JSONEncoder().encode(valid)
    let wire = try JSONSerialization.jsonObject(with: encoded) as! [String: Any]
    let expectedKeys: Set<String> = ["registration_id", "document_binding_verified", "document_binding", "profile", "provenance", "process", "window_id",
        "window_object_ref", "window_frame", "actual_object_ref", "receiver_frame",
        "application_focused_object_ref", "system_focused_object_ref", "ancestry", "scope_complete",
        "profile_eligible", "marked_range_stable_unmarked", "observer_state", "secure", "title", "role",
        "subrole", "enabled", "focused", "value_settable", "description", "marked_range_before", "marked_range_after"]
    let provenanceWire = wire["provenance"] as? [String: Any]
    let documentBindingWire = wire["document_binding"] as? [String: Any]
    let processWire = wire["process"] as? [String: Any]
    try requireTest(Set(wire.keys) == expectedKeys
        && provenanceWire.map { Set($0.keys) } == Set(["identity_boundary", "snapshot_id", "source_sequence",
            "observed_ms", "valid_until_ms", "clock_domain"])
        && processWire.map { Set($0.keys) } == Set(["pid", "start_seconds", "start_micros"])
        && documentBindingWire.map { Set($0.keys) } == Set(["before", "after"])
        && (documentBindingWire?["before"] as? [String: Any]).map { Set($0.keys) } == Set(["status", "api_code", "cf_type_id", "observed_ms", "timely", "registered_path_matches"])
        && wire["document_binding_verified"] as? Bool == true
        && wire["value"] == nil && wire["actual_object_ref"] as? String == "receiver-ref"
        && !String(decoding: encoded, as: UTF8.self).contains("/tmp/owned-document.txt")
        && (wire["title"] as? [String: Any])?["status"] as? String == "value"
        && (wire["description"] as? [String: Any])?["status"] as? String == "unsupported"
        && wire["description"] as? [String: Any] != nil,
        "sidecar keys match Rust DTO, excludes AXValue, and preserves title and description states")
    if let readback {
        var withReadback = valid
        withReadback.value_readback = readback
        let readbackData = try JSONEncoder().encode(withReadback)
        let readbackWire = try JSONSerialization.jsonObject(with: readbackData) as! [String: Any]
        let readbackObject = readbackWire["value_readback"] as? [String: Any]
        let readbackText = String(decoding: readbackData, as: UTF8.self)
        try requireTest(Set(readbackWire.keys) == expectedKeys.union(["value_readback"])
            && readbackObject.map { Set($0.keys) } == Set(["receiver_object_ref",
                "receiver_same_cf_as_actual_object_ref", "observed_ms", "sha256", "utf8_byte_count"])
            && readbackObject?["sha256"] as? String == readback.sha256
            && readbackObject?["utf8_byte_count"] as? Int == 3
            && !readbackText.contains("abc"),
            "readback wire contains only its exact five digest facts and no text")
    } else {
        throw NativeFailure("owned value readback fixture unexpectedly unavailable")
    }
    passed.append("label-free sidecar privacy")
    let changedTitle = projectOwnedDocumentReceiver(ownedReceiverFacts(title: ownedRead("Different title")))
    try requireTest(changedTitle?.actual_object_ref == valid.actual_object_ref
        && changedTitle?.profile_eligible == valid.profile_eligible,
        "title does not participate in receiver identity or eligibility")
    passed.append("title excluded from identity")

    let wrongType = projectOwnedDocumentAttribute(errorCode: AXError.success.rawValue,
        actualCFTypeID: CFStringGetTypeID(), expectedCFTypeID: CFBooleanGetTypeID(),
        decodedValue: Optional<Bool>.none, attempted: true, returnedLate: false)
    let wrongTypeWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(ownedDocumentProperty(wrongType))) as! [String: Any]
    try requireTest(wrongTypeWire["status"] as? String == "decode_failure"
        && wrongTypeWire["api_code"] as? Int32 == 0
        && wrongTypeWire["actual_cf_type_id"] as? UInt64 == UInt64(CFStringGetTypeID())
        && wrongTypeWire["code"] == nil,
        "successful wrong-type AX reply serializes as decode_failure without inventing AX error")
    passed.append("decode failure wire shape")

    func encodingFails<Value: Encodable & Sendable>(_ property: OwnedDocumentProperty<Value>) -> Bool {
        do {
            _ = try JSONEncoder().encode(property)
            return false
        } catch {
            return true
        }
    }
    let invalidProperties = [
        encodingFails(OwnedDocumentProperty<String>(status: .value, value: nil, code: nil,
            actualCFTypeID: nil, reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .unsupported, value: nil, code: nil,
            actualCFTypeID: nil, reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .noValue, value: nil,
            code: AXError.attributeUnsupported.rawValue, actualCFTypeID: nil, reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .failure, value: nil, code: nil,
            actualCFTypeID: nil, reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .failure, value: nil,
            code: AXError.success.rawValue, actualCFTypeID: nil, reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .decodeFailure, value: nil, code: nil,
            actualCFTypeID: UInt64(CFStringGetTypeID()), reason: nil)),
        encodingFails(OwnedDocumentProperty<String>(status: .notAttempted, value: nil, code: nil,
            actualCFTypeID: nil, reason: nil))
    ]
    try requireTest(invalidProperties.allSatisfy { $0 },
                    "invalid typed property states throw instead of emitting null or invented codes")
    passed.append("invalid property encodings rejected")

    let scopeBoundary = SourceIdentityBoundary(namespace: UUID().uuidString.lowercased(), generation: 7)
    let readScope = OwnedDocumentReadScope(snapshotID: "snapshot-7", sourceSequence: 12,
        observedMS: 1_000, validUntilMS: 1_500, clockDomain: "unix_ms")
    let scopedProvenance = readScope.provenance(identityBoundary: scopeBoundary,
        readStartedMS: 1_010, readFinishedMS: 1_100)
    try requireTest(scopedProvenance?.identityBoundary == scopeBoundary
        && scopedProvenance?.snapshotID == "snapshot-7"
        && scopedProvenance?.sourceSequence == 12
        && readScope.provenance(identityBoundary: nil, readStartedMS: 1_010,
                                readFinishedMS: 1_100) == nil
        && readScope.provenance(identityBoundary: scopeBoundary, readStartedMS: 999,
                                readFinishedMS: 1_100) == nil
        && readScope.provenance(identityBoundary: scopeBoundary, readStartedMS: 1_010,
                                readFinishedMS: 1_500) == nil
        && OwnedDocumentReadScope(snapshotID: "snapshot-7", sourceSequence: 0,
            observedMS: 1_000, validUntilMS: 1_500, clockDomain: "unix_ms")
            .provenance(identityBoundary: scopeBoundary, readStartedMS: 1_010,
                        readFinishedMS: 1_100) == nil,
        "caller scope requires real identity, ordered live timestamps, and nonzero sequence")
    let expiringScope = OwnedDocumentReadScope(snapshotID: "snapshot-expiring", sourceSequence: 13,
        observedMS: 1_000, validUntilMS: 1_150, clockDomain: "unix_ms")
    try requireTest(expiringScope.provenance(identityBoundary: scopeBoundary,
            readStartedMS: 1_010, readFinishedMS: 1_100) != nil
        && expiringScope.provenance(identityBoundary: scopeBoundary,
            readStartedMS: 1_010, readFinishedMS: 1_150) == nil,
        "scope that expires after projection is rejected at the final returned read timestamp")
    passed.append("owned read scope provenance bounds")
    return passed
}
