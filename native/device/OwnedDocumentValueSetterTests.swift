import AppKit
import ApplicationServices
import Foundation

func ownedValueFixtureData(_ value: Any) throws -> Data {
    try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
}

func ownedValueFixtureRegistration(_ changes: [String: Any] = [:]) throws -> Data {
    let body: [String: Any] = [
        "version": 1,
        "registration_id": "123e4567-e89b-42d3-a456-426614174000",
        "pid": 1234,
        "start_seconds": 1_700_000_000,
        "start_micros": 123456,
        "executable_path": NativeOwnedDocumentRegistration.textEditExecutablePath,
        "bundle_id": NativeOwnedDocumentRegistration.textEditBundleID,
        "document_path": "/private/tmp/cu-owned-registration-7/document.txt",
        "document_sha256": String(repeating: "a", count: 64),
        "document_dev": 1,
        "document_ino": 2,
        "directory_dev": 3,
        "directory_ino": 4,
        "issued_unix_ms": 10_000,
    ]
    return try ownedValueFixtureData(body.merging(changes) { _, new in new })
}

func ownedValueFixtureAXValue(_ value: Any) -> [String: Any] {
    ["status": "value", "value": value]
}

func ownedValueFixtureMarked(_ object: String, _ observed: UInt64) -> [String: Any] {
    ["receiver_object_ref": object,
     "receiver_same_cf_as_actual_object_ref": true,
     "observed_ms": observed,
     "observation": ownedValueFixtureAXValue(["location": 0, "length": 0])]
}

func ownedValueFixtureProof(
    _ changes: [String: Any] = [:],
    receiverChanges: [String: Any] = [:],
    clockDomain: String = "macos/1/wall"
) throws -> Data {
    let receiver: [String: Any] = [
        "registration_id": "123e4567-e89b-42d3-a456-426614174000",
        "profile": "owned_plain_text_value_setter_v1",
        "provenance": [
            "identity_boundary": ["namespace": "123e4567-e89b-42d3-a456-426614174001", "generation": 3],
            "snapshot_id": "snapshot-owned-1",
            "source_sequence": 2,
            "observed_ms": 100,
            "valid_until_ms": 1_000,
            "clock_domain": clockDomain,
        ],
        "process": ["pid": 1234, "start_seconds": 1_700_000_000, "start_micros": 123456],
        "window_id": "window-4",
        "window_object_ref": "source-window-object",
        "window_frame": ["x": 10.0, "y": 20.0, "width": 500.0, "height": 400.0],
        "actual_object_ref": "source-text-object",
        "receiver_frame": ["x": 20.0, "y": 40.0, "width": 450.0, "height": 350.0],
        "application_focused_object_ref": "source-text-object",
        "system_focused_object_ref": "source-text-object",
        "ancestry": [["object_ref": "source-window-object", "role": "AXWindow"]],
        "scope_complete": true,
        "profile_eligible": true,
        "marked_range_stable_unmarked": true,
        "observer_state": "continuous",
        "secure": false,
        "document_binding_verified": true,
        "document_binding": [
            "before": ["status": "value", "api_code": 0, "cf_type_id": 21,
                       "observed_ms": 110, "timely": true, "registered_path_matches": true],
            "after": ["status": "value", "api_code": 0, "cf_type_id": 21,
                      "observed_ms": 115, "timely": true, "registered_path_matches": true],
        ],
        "title": ownedValueFixtureAXValue("Private test title"),
        "role": ownedValueFixtureAXValue("AXTextArea"),
        "subrole": ownedValueFixtureAXValue(""),
        "enabled": ["status": "unsupported", "code": -25205],
        "focused": ownedValueFixtureAXValue(true),
        "value_settable": ownedValueFixtureAXValue(true),
        "description": ownedValueFixtureAXValue("Private test description"),
        "marked_range_before": ownedValueFixtureMarked("source-text-object", 120),
        "marked_range_after": ownedValueFixtureMarked("source-text-object", 125),
        "value_readback": NSNull(),
    ]
    let body: [String: Any] = [
        "binding": "binding-owned-1",
        "semantic_target": "semantic-target-owned-1",
        "environment_revision": 4,
        "session_epoch": 7,
        "surface": ["id": "main", "epoch": 7],
        "snapshot_observed_ms": 150,
        "receiver": receiver.merging(receiverChanges) { _, new in new },
    ]
    return try ownedValueFixtureData(body.merging(changes) { _, new in new })
}

func runOwnedDocumentValueSetterTests() throws -> Int {
    var passed = 0
    func check(_ condition: @autoclosure () -> Bool, sourceLine: UInt = #line) throws {
        guard condition() else {
            FileHandle.standardError.write(Data("device self-test failed at OwnedDocumentValueSetterTests.swift:\(sourceLine)\n".utf8))
            throw DeviceTestFailure(check: passed + 1)
        }
        passed += 1
    }
    func data(_ value: Any) throws -> Data { try ownedValueFixtureData(value) }
    func registration(_ changes: [String: Any] = [:]) throws -> Data {
        try ownedValueFixtureRegistration(changes)
    }
    func axValue(_ value: Any) -> [String: Any] { ownedValueFixtureAXValue(value) }
    func marked(_ object: String, _ observed: UInt64) -> [String: Any] {
        ownedValueFixtureMarked(object, observed)
    }
    func proof(_ changes: [String: Any] = [:], receiverChanges: [String: Any] = [:]) throws -> Data {
        try ownedValueFixtureProof(changes, receiverChanges: receiverChanges)
    }
    let surface = NativeSurface(id: "main", epoch: 7)
    let validRegistrationData = try registration()
    let validRegistration = try NativeOwnedDocumentRegistration.decodeForPureValidation(validRegistrationData, nowUnixMS: 10_000)
    try check(validRegistration.pid == 1234 && validRegistration.document_path.hasSuffix("/document.txt"))
    try check((try? NativeOwnedDocumentRegistration.decode(
        registration(["registration_id": "not-a-canonical-id"]), nowUnixMS: 10_000)) == nil)
    try check((try? NativeOwnedDocumentRegistration.decode(
        registration(["document_path": "/private/tmp/../document.txt"]), nowUnixMS: 10_000)) == nil)
    try check((try? NativeOwnedDocumentRegistration.decode(
        registration(["document_sha256": "ABCDEF"]), nowUnixMS: 10_000)) == nil)
    try check((try? NativeOwnedDocumentRegistration.decode(
        registration(["issued_unix_ms": 4_000]), nowUnixMS: 10_000)) == nil)
    try check((try? NativeOwnedDocumentRegistration.decodeForPureValidation(
        registration(["pid": "1234"]), nowUnixMS: 10_000)) == nil)
    var extraRegistration = try JSONSerialization.jsonObject(with: validRegistrationData) as! [String: Any]
    extraRegistration["public_setting"] = true
    try check((try? NativeOwnedDocumentRegistration.decode(data(extraRegistration), nowUnixMS: 10_000)) == nil)

    let validProofData = try proof()
    let validProof = try NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)
    try check(validProof.binding == "binding-owned-1" && validProof.surface == surface)
    var unverifiedRegistrationCallbackRan = false
    let unverifiedRegistrationResult = withVerifiedOwnedReceiver(
        registration: validRegistration, proof: validProof, setterValue: "replacement",
        expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", maxAgeMS: 500,
        sourceNowMS: { 200 }, deadlineMS: monotonicMS() + 100,
        onVerified: { _ in unverifiedRegistrationCallbackRan = true; return true })
    try check(unverifiedRegistrationResult == nil && !unverifiedRegistrationCallbackRan)
    try check((try? NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 8, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 7,
        expectedSurface: NativeSurface(id: "other", epoch: 7),
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/2/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 1_000, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(validProofData,
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 99, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: ["secure": true]),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: ["profile_eligible": false]),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: ["registration_id": "other"]),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: [
        "enabled": ["status": "unsupported", "code": -1],
    ]), registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: [
        "value_settable": axValue(false),
    ]), registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check((try? NativeOwnedDocumentBindingProof.decode(try proof(receiverChanges: [
        "marked_range_after": marked("source-text-object", 125).merging([
            "observation": axValue(["location": 1, "length": 0]),
        ]) { _, new in new },
    ]), registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    var extraProof = try JSONSerialization.jsonObject(with: validProofData) as! [String: Any]
    extraProof["execution_grant"] = true
    try check((try? NativeOwnedDocumentBindingProof.decode(data(extraProof),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    var extraNested = try JSONSerialization.jsonObject(with: validProofData) as! [String: Any]
    var nestedReceiver = extraNested["receiver"] as! [String: Any]
    var role = nestedReceiver["role"] as! [String: Any]
    role["unexpected"] = "field"
    nestedReceiver["role"] = role
    extraNested["receiver"] = nestedReceiver
    try check((try? NativeOwnedDocumentBindingProof.decode(data(extraNested),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    var wrongProofType = try JSONSerialization.jsonObject(with: validProofData) as! [String: Any]
    var wrongTypeReceiver = wrongProofType["receiver"] as! [String: Any]
    wrongTypeReceiver["role"] = axValue(true)
    wrongProofType["receiver"] = wrongTypeReceiver
    try check((try? NativeOwnedDocumentBindingProof.decode(data(wrongProofType),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)

    try check(ownedDocumentSetterValueIsValid(String(repeating: "a", count: 2048)))
    try check(!ownedDocumentSetterValueIsValid(String(repeating: "a", count: 2049)))
    try check(ownedDocumentSetterValueIsValid(String(repeating: "é", count: 1024)))
    try check(!ownedDocumentSetterValueIsValid(String(repeating: "é", count: 1025)))
    try check(ownedDocumentSetterValueIsValid("line\nwith\ttabs"))
    try check(!ownedDocumentSetterValueIsValid("control\u{0001}"))

    try check(ownedJSONHasNoDuplicateKeys(Data(#"{"first":1,"second":2}"#.utf8)))
    try check(!ownedJSONHasNoDuplicateKeys(Data(#"{"first":1,"first":2}"#.utf8)))
    try check(!ownedJSONHasNoDuplicateKeys(Data(#"{"first":1,"\u0066irst":2}"#.utf8)))
    try check(!ownedJSONHasNoDuplicateKeys(Data(#"{"outer":{"name":1,"na\u006de":2}}"#.utf8)))
    let escapedDuplicateRegistration = String(decoding: validRegistrationData, as: UTF8.self)
        .replacingOccurrences(of: "\"version\":1",
            with: "\"version\":1,\"ver\\u0073ion\":1")
    try check(!ownedJSONHasNoDuplicateKeys(Data(escapedDuplicateRegistration.utf8)))
    try check((try? NativeOwnedDocumentRegistration.decode(
        Data(escapedDuplicateRegistration.utf8), nowUnixMS: 10_000)) == nil)
    let escapedDuplicateProof = String(decoding: validProofData, as: UTF8.self)
        .replacingOccurrences(of: "\"binding\":\"binding-owned-1\"",
            with: "\"binding\":\"binding-owned-1\",\"bind\\u0069ng\":\"binding-owned-1\"")
    try check(!ownedJSONHasNoDuplicateKeys(Data(escapedDuplicateProof.utf8)))
    try check((try? NativeOwnedDocumentBindingProof.decode(Data(escapedDuplicateProof.utf8),
        registration: validRegistration, expectedSessionEpoch: 7, expectedSurface: surface,
        expectedClockDomain: "macos/1/wall", nowSourceMS: 200, maxAgeMS: 500)) == nil)
    try check(ownedNativeChildListDisposition(errorCode: AXError.noValue.rawValue,
        cfTypeID: nil, rawCount: nil) == .noValueLeaf)
    try check(ownedNativeChildListDisposition(errorCode: AXError.success.rawValue,
        cfTypeID: UInt64(CFArrayGetTypeID()), rawCount: 0) == .typedArray(count: 0))
    try check(ownedNativeChildListDisposition(errorCode: AXError.success.rawValue,
        cfTypeID: UInt64(CFStringGetTypeID()), rawCount: 0) == nil)
    try check(ownedNativeChildListDisposition(errorCode: AXError.attributeUnsupported.rawValue,
        cfTypeID: nil, rawCount: nil) == nil)
    try check((try? NativeOwnedDocumentRegistration.decode(
        Data(#"{"version":1,"ver\u0073ion":1}"#.utf8), nowUnixMS: 10_000)) == nil)
    return passed
}
