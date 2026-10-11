// Read-only AX attribute and observer-registration facts for one owned TextEdit document.
import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import Foundation

private let textEditBundleID = "com.apple.TextEdit"
private let textEditExecutablePath = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
private let documentName = "owned-unicode-fixture.txt"
private let requestLimit = 64 * 1024
private let outputLimit = 256 * 1024

private struct KernelIdentity: Decodable, Equatable {
    let pid: Int32
    let parentPID: Int32
    let startSeconds: Int64
    let startMicroseconds: Int64
    let executablePath: String
    let parentExecutablePath: String?

    enum CodingKeys: String, CodingKey {
        case pid
        case parentPID = "parent_pid"
        case startSeconds = "start_seconds"
        case startMicroseconds = "start_microseconds"
        case executablePath = "executable_path"
        case parentExecutablePath = "parent_executable_path"
    }
}

private struct LaunchReceipt: Decodable {
    let version: Int
    let runID: String
    let launcherID: String
    let launcherIdentity: KernelIdentity
    let targetIdentity: KernelIdentity
    let bundleIdentifier: String
    let executablePath: String
    let documentPath: String
    let documentSHA256: String
    let preexistingTextEditPIDs: [Int32]
    let newPIDExcludedFromPreexistingSet: Bool
    let targetStartedWithinLaunchInterval: Bool
    let createsNewApplicationInstance: Bool
    let addsToRecentItems: Bool
    let activatesNewInstance: Bool
    let arguments: [String]
    let noInputAuthority: Bool

    enum CodingKeys: String, CodingKey {
        case version
        case runID = "run_id"
        case launcherID = "launcher_id"
        case launcherIdentity = "launcher_identity"
        case targetIdentity = "target_identity"
        case bundleIdentifier = "bundle_identifier"
        case executablePath = "executable_path"
        case documentPath = "document_path"
        case documentSHA256 = "document_sha256"
        case preexistingTextEditPIDs = "preexisting_textedit_pids"
        case newPIDExcludedFromPreexistingSet = "new_pid_excluded_from_preexisting_set"
        case targetStartedWithinLaunchInterval = "target_started_within_launch_interval"
        case createsNewApplicationInstance = "creates_new_application_instance"
        case addsToRecentItems = "adds_to_recent_items"
        case activatesNewInstance = "activates_new_instance"
        case arguments
        case noInputAuthority = "no_input_authority"
    }
}

private struct ActivationObservation: Decodable {
    let attempted: Bool
    let attemptCount: Int
    let returnValue: Bool?
    let status: String
    let windowVerified: Bool
    let cgWindowCount: Int
    let axWindowCount: Int
    let cgWindowID: UInt32?
    let framesMatch: Bool
    let systemFocusedApplicationPIDBeforeActivation: Int32?

    enum CodingKeys: String, CodingKey {
        case attempted
        case attemptCount = "attempt_count"
        case returnValue = "return_value"
        case status
        case windowVerified = "window_verified"
        case cgWindowCount = "cg_window_count"
        case axWindowCount = "ax_window_count"
        case cgWindowID = "cg_window_id"
        case framesMatch = "frames_match"
        case systemFocusedApplicationPIDBeforeActivation = "system_focused_application_pid_before_activation"
    }
}

private struct CoverageRequest: Decodable {
    let targetPID: Int32
    let probeParentPID: Int32
    let launcherPID: Int32
    let launcherID: String
    let executablePath: String
    let bundleID: String
    let documentPath: String
    let documentSHA256: String
    let runID: String
    let launchReceipt: LaunchReceipt
    let activationObservation: ActivationObservation?

    enum CodingKeys: String, CodingKey {
        case targetPID = "target_pid"
        case probeParentPID = "probe_parent_pid"
        case launcherPID = "launcher_pid"
        case launcherID = "launcher_id"
        case executablePath = "executable_path"
        case bundleID = "bundle_id"
        case documentPath = "document_path"
        case documentSHA256 = "document_sha256"
        case runID = "run_id"
        case launchReceipt = "launch_receipt"
        case activationObservation = "activation_observation"
    }
}

private struct RawAXNode {
    let element: AXUIElement
    let role: String
    let path: String
}

private struct ScopeRead {
    let facts: [String: Any]
    let verified: Bool
    let application: AXUIElement?
    let systemApplication: AXUIElement?
    let window: AXUIElement?
    let receiver: AXUIElement?
    let chain: [RawAXNode]
    let cgWindowID: UInt32?
    let cgWindowTitle: String?
    let cgWindowFrame: CGRect?
    let axWindowFrame: CGRect?
}

private struct DescendantScan {
    let textAreas: [AXUIElement]
    let visitedCount: Int
    let complete: Bool
    let childReadErrors: [Int32]
    let childNoValueLeafReads: [[String: Any]]
    let childArrayReads: [[String: Any]]
    let childReadsSkippedAtDeadline: Int
    let roleReadFailures: [[String: Any]]
}

private struct AXElementRelationRead {
    let element: AXUIElement?
    let errorCode: Int32?
    let cfTypeID: CFTypeID?
    let attempted: Bool
    let returnedLate: Bool
    let timeout: AXTimeoutAttempt
}

private struct AXElementsRelationRead {
    let elements: [AXUIElement]
    let errorCode: Int32?
    let cfTypeID: CFTypeID?
    let arrayConversionSucceeded: Bool
    let rawElementCount: Int?
    let returnedElementCount: Int
    let truncated: Bool
    let attempted: Bool
    let returnedLate: Bool
    let timeout: AXTimeoutAttempt
}

private struct AXValueRead {
    let value: CFTypeRef?
    let errorCode: Int32?
    let attempted: Bool
    let returnedLate: Bool
    let timeout: AXTimeoutAttempt
}

private struct AXTimeoutAttempt {
    let attempted: Bool
    let seconds: Double?
    let errorCode: Int32?
    let returnedLate: Bool

    var permitsNextCall: Bool {
        attempted && errorCode == AXError.success.rawValue && !returnedLate
    }
}

private func noopObserverCallback(_ observer: AXObserver, _ element: AXUIElement,
                                  _ notification: CFString, _ context: UnsafeMutableRawPointer?) {}

@main
struct OwnedTextEditCoverageProbe {
    @MainActor private static var deadline = DispatchTime.now().uptimeNanoseconds

    @MainActor
    static func main() {
        let mode = CommandLine.arguments.dropFirst().first ?? "--coverage"
        guard ["--protocol-check", "--window-ready-only", "--coverage"].contains(mode),
              let requestData = readRequestLine(),
              let request = try? JSONDecoder().decode(CoverageRequest.self, from: requestData),
              validReceipt(request), validOwnedDocument(request) else {
            emit(["kind": "probe_error", "error": "invalid request, launch receipt or run-owned document"])
            Darwin.exit(2)
        }
        if mode == "--protocol-check" {
            emit(protocolCheck(request))
            return
        }
        guard request.probeParentPID == getppid(),
              pathForPID(getpid())?.hasSuffix("/OwnedTextEditCoverageProbe") == true else {
            emit(["kind": "probe_error", "error": "probe parent or executable identity mismatch"])
            Darwin.exit(2)
        }
        deadline = DispatchTime.now().uptimeNanoseconds + 7_000_000_000
        currentTargetPID = request.targetPID
        let result = mode == "--window-ready-only" ? windowReadiness(request) : coverageRead(request)
        emit(result)
    }

    private static func readRequestLine() -> Data? {
        var bytes: [UInt8] = []
        bytes.reserveCapacity(4096)
        var chunk = [UInt8](repeating: 0, count: 4096)
        while bytes.count <= requestLimit {
            let count = chunk.withUnsafeMutableBytes { buffer in
                Darwin.read(STDIN_FILENO, buffer.baseAddress, buffer.count)
            }
            if count <= 0 { break }
            for byte in chunk.prefix(count) {
                if byte == 10 { return Data(bytes) }
                bytes.append(byte)
                if bytes.count > requestLimit { return nil }
            }
        }
        return bytes.isEmpty ? nil : Data(bytes)
    }

    private static func protocolCheck(_ request: CoverageRequest) -> [String: Any] {
        let cases: [(Int32, String)] = [
            (0, "success"), (-25205, "attribute_unsupported"),
            (-25207, "notification_unsupported"), (-25212, "no_value"), (-25200, "failure")
        ]
        let protocolTimeout = AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false)
        let emptyValidArray = AXElementsRelationRead(
            elements: [], errorCode: AXError.success.rawValue, cfTypeID: CFArrayGetTypeID(),
            arrayConversionSucceeded: true, rawElementCount: 0, returnedElementCount: 0, truncated: false,
            attempted: true, returnedLate: false, timeout: protocolTimeout)
        let oversizedArray = AXElementsRelationRead(
            elements: Array(repeating: AXUIElementCreateSystemWide(), count: 64),
            errorCode: AXError.success.rawValue, cfTypeID: CFArrayGetTypeID(),
            arrayConversionSucceeded: true, rawElementCount: 65, returnedElementCount: 64, truncated: true,
            attempted: true, returnedLate: false, timeout: protocolTimeout)
        let wrongTypeArray = AXElementsRelationRead(
            elements: [], errorCode: AXError.success.rawValue, cfTypeID: CFStringGetTypeID(),
            arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
            attempted: true, returnedLate: false, timeout: protocolTimeout)
        let noValueArray = AXElementsRelationRead(
            elements: [], errorCode: AXError.noValue.rawValue, cfTypeID: nil,
            arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
            attempted: true, returnedLate: false, timeout: protocolTimeout)
        let arrayShapeCasesPass = verifiedAXRead(emptyValidArray)
            && !verifiedAXRead(oversizedArray)
            && !verifiedAXRead(wrongTypeArray)
            && !verifiedAXRead(noValueArray)
        let classifications = cases.map { code, expected in
            ["error_code": code, "classification": classify(code),
             "is_success": code == 0, "expected": expected] as [String: Any]
        }
        let passed = cases.allSatisfy { classify($0.0) == $0.1 }
            && request.launchReceipt.targetIdentity.pid == request.targetPID
            && request.launchReceipt.launcherIdentity.pid == request.launcherPID
            && request.launchReceipt.runID == request.runID
            && request.launchReceipt.launcherID == request.launcherID
            && timeoutSecondsForRemainingBudget(40_000_000) == 0.04
            && abs((timeoutSecondsForRemainingBudget(10_000_000) ?? -1) - 0.01) < 0.000_001
            && timeoutSecondsForRemainingBudget(0) == nil
            && canCallAXAfterTimeoutSetup(
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false),
                now: 100, deadline: 140_000_100)
            && !canCallAXAfterTimeoutSetup(
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false),
                now: 100, deadline: 30_000_100)
            && !canCallAXAfterTimeoutSetup(
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: -25200, returnedLate: false),
                now: 100, deadline: 140_000_100)
            && !canCallAXAfterTimeoutSetup(
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: true),
                now: 100, deadline: 140_000_100)
            && apiSuccessWithinDeadline(errorCode: 0, returnedAt: 99, deadline: 100)
            && !apiSuccessWithinDeadline(errorCode: 0, returnedAt: 100, deadline: 100)
            && !apiSuccessWithinDeadline(errorCode: -25207, returnedAt: 99, deadline: 100)
            && optionalChildrenNoValueIsLeaf(AXElementsRelationRead(
                elements: [], errorCode: AXError.noValue.rawValue, cfTypeID: nil,
                arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
                attempted: true, returnedLate: false,
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false)))
            && !optionalChildrenNoValueIsLeaf(AXElementsRelationRead(
                elements: [], errorCode: AXError.noValue.rawValue, cfTypeID: nil,
                arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
                attempted: false, returnedLate: false, timeout: noTimeoutAttempt()))
            && !optionalChildrenNoValueIsLeaf(AXElementsRelationRead(
                elements: [], errorCode: AXError.noValue.rawValue, cfTypeID: nil,
                arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
                attempted: true, returnedLate: true,
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false)))
            && !optionalChildrenNoValueIsLeaf(AXElementsRelationRead(
                elements: [], errorCode: AXError.attributeUnsupported.rawValue, cfTypeID: nil,
                arrayConversionSucceeded: false, rawElementCount: nil, returnedElementCount: 0, truncated: false,
                attempted: true, returnedLate: false,
                timeout: AXTimeoutAttempt(attempted: true, seconds: 0.04, errorCode: 0, returnedLate: false)))
            && arrayShapeCasesPass
        return ["kind": "protocol_check", "decoded": true,
                "receipt_fields_match": passed,
                "unsupported_remains_non_success": cases.filter { $0.0 != 0 }.allSatisfy { classify($0.0) != "success" },
                "timeout_budget_and_late_success_cases_passed": passed,
                "timeout_setup_failure_or_late_return_blocks_query": passed,
                "optional_children_no_value_leaf_cases_passed": passed,
                "children_no_value_is_not_relabelled_ax_success": classify(AXError.noValue.rawValue) != "success",
                "children_array_shape_cases_passed": arrayShapeCasesPass,
                "array_shape_errors_do_not_read_as_success": arrayShapeCasesPass,
                "classifications": classifications, "passed": passed]
    }

    private static func classify(_ code: Int32) -> String {
        switch code {
        case 0: return "success"
        case -25205: return "attribute_unsupported"
        case -25207: return "notification_unsupported"
        case -25212: return "no_value"
        default: return "failure"
        }
    }

    private static func timeoutSecondsForRemainingBudget(_ remainingNs: UInt64) -> Double? {
        guard remainingNs > 0 else { return nil }
        return min(0.04, Double(remainingNs) / 1_000_000_000)
    }

    private static func apiSuccessWithinDeadline(errorCode: Int32, returnedAt: UInt64,
                                                 deadline: UInt64) -> Bool {
        errorCode == AXError.success.rawValue && returnedAt < deadline
    }

    private static func canCallAXAfterTimeoutSetup(timeout: AXTimeoutAttempt,
                                                   now: UInt64, deadline: UInt64) -> Bool {
        guard timeout.permitsNextCall, now < deadline, let seconds = timeout.seconds else { return false }
        let remaining = Double(deadline - now) / 1_000_000_000
        return seconds <= remaining
    }

    @MainActor
    private static func configureMessagingTimeout(_ element: AXUIElement) -> AXTimeoutAttempt {
        let startedAt = uptimeNow()
        guard let seconds = timeoutSecondsForRemainingBudget(deadline > startedAt ? deadline - startedAt : 0) else {
            return AXTimeoutAttempt(attempted: false, seconds: nil, errorCode: nil, returnedLate: false)
        }
        let error = AXUIElementSetMessagingTimeout(element, Float(seconds))
        let returnedAt = uptimeNow()
        return AXTimeoutAttempt(attempted: true, seconds: seconds, errorCode: error.rawValue,
                                returnedLate: returnedAt >= deadline)
    }

    private static func timeoutObject(_ timeout: AXTimeoutAttempt) -> [String: Any] {
        ["api_call_attempted": timeout.attempted,
         "configured_seconds": timeout.seconds as Any? ?? NSNull(),
         "error_code": timeout.errorCode as Any? ?? NSNull(),
         "returned_late": timeout.returnedLate,
         "permits_next_call": timeout.permitsNextCall]
    }

    private static func noTimeoutAttempt() -> AXTimeoutAttempt {
        AXTimeoutAttempt(attempted: false, seconds: nil, errorCode: nil, returnedLate: false)
    }

    private static func verifiedAXRead(_ read: AXElementRelationRead) -> Bool {
        read.attempted && read.errorCode == AXError.success.rawValue
            && !read.returnedLate && read.timeout.permitsNextCall
    }

    private static func verifiedAXRead(_ read: AXElementsRelationRead) -> Bool {
        read.attempted && read.errorCode == AXError.success.rawValue
            && !read.returnedLate && read.timeout.permitsNextCall
            && read.arrayConversionSucceeded && !read.truncated
            && read.rawElementCount.map { $0 == read.returnedElementCount } == true
            && read.returnedElementCount == read.elements.count
    }

    private static func arrayRelationObject(_ read: AXElementsRelationRead) -> [String: Any] {
        ["api_call_attempted": read.attempted,
         "error_code": read.errorCode as Any? ?? NSNull(),
         "cf_type_id": read.cfTypeID as Any? ?? NSNull(),
         "array_conversion_succeeded": read.arrayConversionSucceeded,
         "raw_element_count": read.rawElementCount as Any? ?? NSNull(),
         "returned_element_count": read.returnedElementCount,
         "truncated": read.truncated,
         "returned_late": read.returnedLate,
         "timeout_setup": timeoutObject(read.timeout)]
    }

    private static func compactScopeAttemptFacts(_ facts: [String: Any]) -> [String: Any] {
        let keys = [
            "target_pid", "process_identity", "process_identity_matches_receipt",
            "application_element_pid", "window_element_pid", "bundle_id", "frontmost_application_pid",
            "system_focused_application_pid", "system_focused_application_api_call_attempted",
            "system_focused_application_error", "system_focused_application_returned_late",
            "system_focused_application_timeout", "system_focused_application_same_cf_as_target_application",
            "system_focused_receiver_api_call_attempted",
            "system_focused_receiver_error", "system_focused_receiver_returned_late",
            "system_focused_receiver_timeout", "application_focused_receiver_api_call_attempted",
            "application_focused_receiver_error", "application_focused_receiver_returned_late",
            "application_focused_receiver_timeout", "application_focused_window_api_call_attempted",
            "application_focused_window_error", "application_focused_window_returned_late",
            "application_focused_window_timeout", "system_receiver_same_cf_as_application_receiver",
            "receiver_role", "receiver_role_read", "cg_window_count", "cg_window_id",
            "cg_window_title_matches_owned_document", "ax_window_count", "ax_windows_api_call_attempted",
            "ax_windows_error", "ax_windows_returned_late", "ax_windows_timeout", "ax_windows_cf_type_id",
            "ax_windows_array_conversion_succeeded", "ax_windows_raw_element_count",
            "ax_windows_returned_element_count", "ax_windows_truncated", "window_in_app_count",
            "ax_window_title_matches_owned_document", "ax_window_title_read", "cg_ax_window_frames_match",
            "receiver_is_in_unique_window_text_area_list", "owned_window_tree_scan_node_count",
            "owned_window_tree_scan_complete", "owned_window_tree_child_read_errors",
            "owned_window_tree_children_no_value_leaf_reads", "owned_window_tree_children_array_reads",
            "owned_window_tree_child_reads_skipped_at_deadline",
            "owned_window_tree_role_read_failures", "receiver_window_relation_same_cf",
            "receiver_ancestor_memberships_verified", "receiver_ancestor_cf_relations", "scope_verified"
        ]
        var result = [String: Any]()
        for key in keys {
            if let value = facts[key] { result[key] = value }
        }
        if let failures = facts["owned_window_tree_role_read_failures"] as? [[String: Any]] {
            result["owned_window_tree_role_read_failure_count"] = failures.count
            result["owned_window_tree_role_read_failure_codes"] = failures.prefix(32).map { failure in
                let read = failure["role_read"] as? [String: Any] ?? [:]
                return ["node_index": failure["node_index"] as Any? ?? NSNull(),
                        "api_call_attempted": read["api_call_attempted"] as Any? ?? NSNull(),
                        "error_code": read["error_code"] as Any? ?? NSNull(),
                        "returned_late": read["returned_late"] as Any? ?? NSNull(),
                        "timeout_setup": read["timeout_setup"] as Any? ?? NSNull()]
            }
            result.removeValue(forKey: "owned_window_tree_role_read_failures")
        }
        return result
    }

    private static func optionalChildrenNoValueIsLeaf(_ read: AXElementsRelationRead) -> Bool {
        read.attempted && read.errorCode == AXError.noValue.rawValue
            && !read.returnedLate && read.timeout.permitsNextCall
    }

    @MainActor
    private static func windowReadiness(_ request: CoverageRequest) -> [String: Any] {
        let identity = processIdentity(request.targetPID)
        let cg = cgWindows(pid: request.targetPID)
        let application = AXUIElementCreateApplication(pid_t(request.targetPID))
        let windowsRead = relatedElements(application, kAXWindowsAttribute as String)
        let windows = windowsRead.elements
        let axFacts = windows.enumerated().map { index, window in
            let role: Any = rawAttribute(window, kAXRoleAttribute as String)["string_value"] as Any? ?? NSNull()
            let windowFrame: Any = frame(window).map(rectObject) as Any? ?? NSNull()
            return ["index": index, "role": role, "window_pid": elementPID(window) as Any? ?? NSNull(), "frame": windowFrame,
                    "title_matches_owned_document": titleMatches(window, request: request)]
        }
        let match = windows.first { axWindow in cg?.contains { cgWindow in sameFrame(frame(axWindow), cgBounds(cgWindow)) } == true }
        let axTitle = match.flatMap { attributeString($0, kAXTitleAttribute as String) }
        let system = AXUIElementCreateSystemWide()
        let focusRead = relatedElement(system, kAXFocusedApplicationAttribute as String)
        let focusPID = focusRead.element.flatMap(elementPID)
        let cgTitleMatches = cg?.first.flatMap(cgTitle)?.contains(documentName) ?? false
        let framesMatch = match != nil && cg?.count == 1 && windows.count == 1
            && sameFrame(frame(match!), cgBounds(cg?.first ?? [:]))
        let verified = uptimeNow() < deadline && identity == request.launchReceipt.targetIdentity
            && verifiedAXRead(windowsRead)
            && focusRead.attempted && focusRead.errorCode == AXError.success.rawValue
            && !focusRead.returnedLate && focusRead.timeout.permitsNextCall
            && windows.count == 1 && cg?.count == 1 && framesMatch
            && windows.first.flatMap(elementPID) == request.targetPID
            && cgTitleMatches && (match.map { titleMatches($0, request: request) } ?? false)
        return ["kind": "textedit_coverage_window_readiness", "run_id": request.runID,
                "target_pid": request.targetPID, "window_verified": verified,
                "process_identity_before": identityObject(identity), "process_identity_revalidated": identity == request.launchReceipt.targetIdentity,
                "ax_windows_call_attempted": windowsRead.attempted,
                "ax_windows_error": windowsRead.errorCode as Any? ?? NSNull(),
                "ax_windows_cf_type_id": windowsRead.cfTypeID as Any? ?? NSNull(),
                "ax_windows_array_conversion_succeeded": windowsRead.arrayConversionSucceeded,
                "ax_windows_raw_element_count": windowsRead.rawElementCount as Any? ?? NSNull(),
                "ax_windows_returned_element_count": windowsRead.returnedElementCount,
                "ax_windows_truncated": windowsRead.truncated,
                "ax_windows_returned_late": windowsRead.returnedLate,
                "ax_windows_timeout": timeoutObject(windowsRead.timeout),
                "system_focused_application_call_attempted": focusRead.attempted,
                "system_focused_application_error": focusRead.errorCode as Any? ?? NSNull(),
                "system_focused_application_cf_type_id": focusRead.cfTypeID as Any? ?? NSNull(),
                "system_focused_application_returned_late": focusRead.returnedLate,
                "system_focused_application_timeout": timeoutObject(focusRead.timeout),
                "system_focused_application_pid_at_window_check": focusPID as Any? ?? NSNull(),
                "ax_window_count": windows.count, "ax_windows": axFacts,
                "cg_window_count": cg?.count ?? 0, "cg_window_id": cg?.first.flatMap(cgID) as Any? ?? NSNull(),
                "cg_title": cg?.first.flatMap(cgTitle) as Any? ?? NSNull(),
                "cg_title_matches_owned_document": cgTitleMatches,
                "ax_title": axTitle as Any? ?? NSNull(),
                "frames_match": framesMatch,
                "no_input_authority": true]
    }

    @MainActor
    private static func coverageRead(_ request: CoverageRequest) -> [String: Any] {
        let runStarted = DispatchTime.now().uptimeNanoseconds
        let processBefore = processIdentity(request.targetPID)
        let launcherBefore = processIdentity(request.launcherPID)
        var report: [String: Any] = [
            "kind": "textedit_owned_observer_attribute_coverage",
            "run_id": request.runID, "launcher_id": request.launcherID,
            "target_pid": request.targetPID, "bundle_id": request.bundleID,
            "document_path": request.documentPath, "document_sha256": request.documentSHA256,
            "document_verified": validOwnedDocument(request),
            "launch_receipt_verified": validReceipt(request),
            "activation_observation": activationObject(request.activationObservation),
            "ax_process_trusted": AXIsProcessTrusted(),
            "probe_parent_pid": getppid(),
            "probe_identity_before": identityObject(processIdentity(getpid())),
            "process_identity_before": identityObject(processBefore),
            "launcher_identity_before": identityObject(launcherBefore),
            "no_input_authority": true,
            "ax_set_value_calls": 0, "ax_press_calls": 0, "set_focus_calls": 0,
            "cg_post_event_calls": 0
        ]

        let scopeWaitStarted = uptimeNow()
        let scopeAttemptLimit = 8
        var scopeAttempts = [[String: Any]]()
        var baselineRead: ScopeRead?
        var lastScopeRead: ScopeRead?
        var scopeWaitStopReason = "original_7s_deadline_elapsed"
        while uptimeNow() < deadline && scopeAttempts.count < scopeAttemptLimit {
            let attemptStarted = uptimeNow()
            let read = readScope(request)
            let attemptFinished = uptimeNow()
            lastScopeRead = read
            let attempt: [String: Any] = [
                "attempt": scopeAttempts.count + 1,
                "elapsed_uptime_ms": (attemptFinished - runStarted) / 1_000_000,
                "read_elapsed_ms": (attemptFinished - attemptStarted) / 1_000_000,
                "scope_verified": read.verified,
                "scope_facts": compactScopeAttemptFacts(read.facts)
            ]
            scopeAttempts.append(attempt)
            if read.verified {
                baselineRead = read
                scopeWaitStopReason = "complete_owned_focused_scope_verified"
                break
            }
            if uptimeNow() >= deadline { break }
            Thread.sleep(forTimeInterval: 0.05)
        }
        if baselineRead == nil && scopeAttempts.count == scopeAttemptLimit {
            scopeWaitStopReason = "scope_readiness_attempt_limit"
        }
        report["scope_before_wait_attempts"] = scopeAttempts
        report["scope_before_wait_attempt_count"] = scopeAttempts.count
        report["scope_before_wait_elapsed_ms"] = (uptimeNow() - scopeWaitStarted) / 1_000_000
        report["scope_before_wait_attempt_limit"] = scopeAttemptLimit
        report["scope_before_wait_stop_reason"] = scopeWaitStopReason

        guard let before = baselineRead,
              before.verified,
              let application = before.application,
              let window = before.window,
              let receiver = before.receiver,
              uptimeNow() < deadline else {
            report["scope_before"] = lastScopeRead?.facts as Any? ?? NSNull()
            report["status"] = "scope_not_verified"
            report["scope_verified"] = false
            report["observer_subscriptions_attempted"] = false
            report["scope_after"] = NSNull()
            report["scope_after_api_reads_attempted"] = false
            let processAfter = processIdentity(request.targetPID)
            let launcherAfter = processIdentity(request.launcherPID)
            report["process_identity_after"] = identityObject(processAfter)
            report["process_identity_unchanged"] = processAfter == processBefore && processAfter == request.launchReceipt.targetIdentity
            report["launcher_identity_after"] = identityObject(launcherAfter)
            report["launcher_identity_unchanged"] = launcherAfter == launcherBefore && launcherAfter == request.launchReceipt.launcherIdentity
            report["receiver_same_cf_before_after"] = false
            report["window_same_cf_before_after"] = false
            report["system_focused_application_same_cf_before_after"] = false
            report["cg_window_id_same_before_after"] = false
            report["scope_unchanged_before_after"] = false
            report["deadline_exceeded"] = uptimeNow() >= deadline
            report["elapsed_uptime_ms"] = (uptimeNow() - runStarted) / 1_000_000
            return report
        }

        report["scope_before"] = before.facts
        report["scope_verified"] = true
        report["ax_attribute_reads"] = before.facts["ax_attribute_reads"] ?? []
        report["receiver_value_settable"] = rawSettable(receiver, kAXValueAttribute as String)
        report["receiver_marked_range"] = markedRange(receiver)
        report["observer_registration"] = registerObservers(
            pid: request.targetPID, application: application, window: window,
            chain: before.chain)

        let after = readScope(request)
        report["scope_after"] = after.facts
        let processAfter = processIdentity(request.targetPID)
        let launcherAfter = processIdentity(request.launcherPID)
        report["probe_identity_after"] = identityObject(processIdentity(getpid()))
        report["probe_parent_matches_request"] = getppid() == request.probeParentPID
        report["process_identity_after"] = identityObject(processAfter)
        report["launcher_identity_after"] = identityObject(launcherAfter)
        report["process_identity_unchanged"] = processAfter == processBefore && processAfter == request.launchReceipt.targetIdentity
        report["launcher_identity_unchanged"] = launcherAfter == launcherBefore && launcherAfter == request.launchReceipt.launcherIdentity
        let receiverSameCF = before.receiver != nil && after.receiver.map { CFEqual(before.receiver!, $0) } == true
        let windowSameCF = before.window != nil && after.window.map { CFEqual(before.window!, $0) } == true
        let systemApplicationSameCF = before.systemApplication != nil
            && after.systemApplication.map { CFEqual(before.systemApplication!, $0) } == true
        let cgWindowSame = before.cgWindowID != nil && before.cgWindowID == after.cgWindowID
        let cgWindowFrameSame = sameFrame(before.cgWindowFrame, after.cgWindowFrame)
        let axWindowFrameSame = sameFrame(before.axWindowFrame, after.axWindowFrame)
        let cgWindowTitleSame = before.cgWindowTitle != nil && before.cgWindowTitle == after.cgWindowTitle
        report["receiver_same_cf_before_after"] = receiverSameCF
        report["window_same_cf_before_after"] = windowSameCF
        report["system_focused_application_same_cf_before_after"] = systemApplicationSameCF
        report["cg_window_id_same_before_after"] = cgWindowSame
        report["cg_window_frame_same_before_after"] = cgWindowFrameSame
        report["ax_window_frame_same_before_after"] = axWindowFrameSame
        report["cg_window_title_same_before_after"] = cgWindowTitleSame
        report["scope_unchanged_before_after"] = before.verified && after.verified
            && receiverSameCF && windowSameCF && systemApplicationSameCF && cgWindowSame
            && cgWindowFrameSame && axWindowFrameSame && cgWindowTitleSame
        report["deadline_exceeded"] = uptimeNow() >= deadline
        report["elapsed_uptime_ms"] = (uptimeNow() - runStarted) / 1_000_000
        report["observer_subscriptions_are_delivery_proof"] = false
        report["note"] = "AX observer registration return codes prove registration outcomes only; this probe did not wait for notification delivery."
        report["status"] = report["scope_unchanged_before_after"] as? Bool == true
            ? "owned_receiver_scope_verified" : "post_scope_revalidation_failed"
        return report
    }

    @MainActor
    private static func readScope(_ request: CoverageRequest) -> ScopeRead {
        let app = AXUIElementCreateApplication(pid_t(request.targetPID))
        let system = AXUIElementCreateSystemWide()
        let systemApplicationRead = relatedElement(system, kAXFocusedApplicationAttribute as String)
        let systemApplication = systemApplicationRead.element
        let systemAppPID = systemApplication.flatMap(elementPID)
        let systemReceiverRead = systemApplication.map {
            relatedElement($0, kAXFocusedUIElementAttribute as String)
        } ?? AXElementRelationRead(element: nil, errorCode: nil, cfTypeID: nil, attempted: false,
                                   returnedLate: false, timeout: noTimeoutAttempt())
        let systemReceiver = systemReceiverRead.element
        let ownReceiverRead = relatedElement(app, kAXFocusedUIElementAttribute as String)
        let ownReceiver = ownReceiverRead.element
        let ownFocusedWindowRead = relatedElement(app, kAXFocusedWindowAttribute as String)
        let ownFocusedWindow = ownFocusedWindowRead.element
        let windowsRead = relatedElements(app, kAXWindowsAttribute as String)
        let windows = windowsRead.elements
        let cg = cgWindows(pid: request.targetPID)
        let window = windows.first { axWindow in cg?.contains { sameFrame(frame(axWindow), cgBounds($0)) } == true }
        let treeScan = window.map { descendants(role: kAXTextAreaRole as String, of: $0, limit: 32, depthLimit: 8) }
        let textAreas = treeScan?.textAreas ?? []
        let receiverRoleRead = systemReceiver.map { rawAttribute($0, kAXRoleAttribute as String) }
        let receiverRole = receiverRoleRead?["string_value"] as? String
        var chain = [RawAXNode]()
        var chainRelations = [[String: Any]]()
        if let systemReceiver, receiverRole == kAXTextAreaRole as String, let window {
            chain.append(RawAXNode(element: systemReceiver, role: receiverRole!, path: "receiver"))
            var current = systemReceiver
            for index in 0..<8 where !CFEqual(current, window) {
                let parentRead = relatedElement(current, kAXParentAttribute as String)
                let parent = parentRead.element
                guard let parent else {
                    chainRelations.append(["path": chain.last?.path ?? "unknown",
                                           "parent_api_call_attempted": parentRead.attempted,
                                           "parent_returned_late": parentRead.returnedLate,
                                           "parent_timeout_setup": timeoutObject(parentRead.timeout),
                                           "parent_error_code": parentRead.errorCode as Any? ?? NSNull(),
                                           "parent_cf_type_id": parentRead.cfTypeID as Any? ?? NSNull()])
                    break
                }
                let parentRole = rawAttribute(parent, kAXRoleAttribute as String)["string_value"] as? String ?? "Unknown"
                let siblingsRead = relatedElements(parent, kAXChildrenAttribute as String)
                let membership = siblingsRead.elements.filter { CFEqual($0, current) }.count
                let path = parentRole == kAXWindowRole as String ? "window" : "ancestor_\(index + 1)"
                chainRelations.append(["path": path, "parent_api_call_attempted": parentRead.attempted,
                                      "parent_returned_late": parentRead.returnedLate,
                                      "parent_timeout_setup": timeoutObject(parentRead.timeout),
                                      "parent_error_code": parentRead.errorCode as Any? ?? NSNull(),
                                      "parent_cf_type_id": parentRead.cfTypeID as Any? ?? NSNull(),
                                      "children_api_call_attempted": siblingsRead.attempted,
                                      "children_returned_late": siblingsRead.returnedLate,
                                      "children_timeout_setup": timeoutObject(siblingsRead.timeout),
                                      "children_error_code": siblingsRead.errorCode as Any? ?? NSNull(),
                                      "children_cf_type_id": siblingsRead.cfTypeID as Any? ?? NSNull(),
                                      "children_array_conversion_succeeded": siblingsRead.arrayConversionSucceeded,
                                      "children_raw_element_count": siblingsRead.rawElementCount as Any? ?? NSNull(),
                                      "children_returned_element_count": siblingsRead.returnedElementCount,
                                      "children_truncated": siblingsRead.truncated,
                                      "actual_cf_membership_count": membership,
                                      "same_cf_parent_relation": verifiedAXRead(parentRead)
                                        && verifiedAXRead(siblingsRead) && membership == 1,
                                      "parent_pid_matches_target": elementPID(parent) == request.targetPID])
                chain.append(RawAXNode(element: parent, role: parentRole, path: path))
                current = parent
                if CFEqual(current, window) { break }
            }
        }
        let relationReachesWindow = chain.contains { node in
            node.path == "window" && window.map { CFEqual(node.element, $0) } == true
        }
        let windowInAppCount = window.map { candidate in windows.filter { CFEqual($0, candidate) }.count } ?? 0
        let receiverInTreeCount = systemReceiver.map { receiver in textAreas.filter { CFEqual($0, receiver) }.count } ?? 0
        let cgWindow = cg?.first
        let cgID = cgWindow.flatMap(cgID)
        let cgWindowTitle = cgWindow.flatMap(cgTitle)
        let cgWindowFrame = cgWindow.flatMap(cgBounds)
        let axWindowFrame = window.flatMap(frame)
        let boundsMatch = window != nil && cgWindow.map { sameFrame(frame(window!), cgBounds($0)) } == true
        let windowTitleRead = window.map { rawAttribute($0, kAXTitleAttribute as String, path: "window", request: request) }
        let titleMatch = windowTitleRead?["owned_document_name_matches"] as? Bool == true
        let cgTitleMatch = cgWindowTitle?.contains(URL(fileURLWithPath: request.documentPath).lastPathComponent) ?? false
        let frontmostPID = NSWorkspace.shared.frontmostApplication.map { Int32($0.processIdentifier) }
        let appBundle = NSRunningApplication(processIdentifier: pid_t(request.targetPID))?.bundleIdentifier
        let process = processIdentity(request.targetPID)
        let appPID = elementPID(app)
        let windowPID = window.flatMap(elementPID)
        let receiptMatches = process == request.launchReceipt.targetIdentity
            && request.targetPID == request.launchReceipt.targetIdentity.pid
            && normalizedPath(request.executablePath) == normalizedPath(textEditExecutablePath)
            && appBundle == textEditBundleID
        let systemReceiverSameOwn = systemReceiver != nil && ownReceiver.map { CFEqual(systemReceiver!, $0) } == true
        let systemApplicationSameOwn = systemApplication.map { CFEqual($0, app) } == true
        let focusedWindowSame = ownFocusedWindow != nil && window.map { CFEqual(ownFocusedWindow!, $0) } == true
        let appIsFocused = systemApplicationRead.attempted && systemApplicationRead.errorCode == AXError.success.rawValue
            && !systemApplicationRead.returnedLate && systemApplicationRead.timeout.permitsNextCall
            && systemAppPID == request.targetPID
            && frontmostPID == request.targetPID
        let uniqueOwnedWindow = verifiedAXRead(windowsRead)
            && windows.count == 1 && cg?.count == 1
            && window != nil && windowPID == request.targetPID && windowInAppCount == 1
            && boundsMatch && titleMatch && cgTitleMatch && cgID != nil
        let chainMembershipsVerified = !chainRelations.isEmpty && chainRelations.count == chain.count - 1
            && chainRelations.allSatisfy { ($0["same_cf_parent_relation"] as? Bool) == true
                && ($0["parent_pid_matches_target"] as? Bool) == true }
        let uniqueFocusedReceiver = verifiedAXRead(ownReceiverRead) && verifiedAXRead(systemReceiverRead)
            && verifiedAXRead(ownFocusedWindowRead)
            && systemReceiver != nil && elementPID(systemReceiver!) == request.targetPID
            && systemReceiverSameOwn && systemApplicationSameOwn && receiverRole == kAXTextAreaRole as String
            && textAreas.count == 1 && treeScan?.complete == true && receiverInTreeCount == 1
            && focusedWindowSame && relationReachesWindow && chainMembershipsVerified
        let verified = uptimeNow() < deadline && appPID == request.targetPID
            && receiptMatches && appIsFocused && uniqueOwnedWindow && uniqueFocusedReceiver
        var attributes = chain.map { node in
            ["path": node.path, "role_hint": node.role, "attributes": attributeFacts(node.element, path: node.path, request: request)] as [String: Any]
        }
        attributes.append(["path": "application", "role_hint": "AXApplication",
                           "attributes": attributeFacts(app, path: "application", request: request)])
        let facts: [String: Any] = [
            "target_pid": request.targetPID,
            "process_identity": identityObject(process), "process_identity_matches_receipt": receiptMatches,
            "application_element_pid": appPID as Any? ?? NSNull(), "window_element_pid": windowPID as Any? ?? NSNull(),
            "bundle_id": appBundle as Any? ?? NSNull(),
            "frontmost_application_pid": frontmostPID as Any? ?? NSNull(),
            "system_focused_application_pid": systemAppPID as Any? ?? NSNull(),
            "system_focused_application_api_call_attempted": systemApplicationRead.attempted,
            "system_focused_application_error": systemApplicationRead.errorCode as Any? ?? NSNull(),
            "system_focused_application_returned_late": systemApplicationRead.returnedLate,
            "system_focused_application_timeout": timeoutObject(systemApplicationRead.timeout),
            "system_focused_application_cf_type_id": systemApplicationRead.cfTypeID as Any? ?? NSNull(),
            "system_focused_application_same_cf_as_target_application": systemApplicationSameOwn,
            "system_focused_receiver_api_call_attempted": systemReceiverRead.attempted,
            "system_focused_receiver_error": systemReceiverRead.errorCode as Any? ?? NSNull(),
            "system_focused_receiver_returned_late": systemReceiverRead.returnedLate,
            "system_focused_receiver_timeout": timeoutObject(systemReceiverRead.timeout),
            "system_focused_receiver_cf_type_id": systemReceiverRead.cfTypeID as Any? ?? NSNull(),
            "application_focused_receiver_api_call_attempted": ownReceiverRead.attempted,
            "application_focused_receiver_error": ownReceiverRead.errorCode as Any? ?? NSNull(),
            "application_focused_receiver_returned_late": ownReceiverRead.returnedLate,
            "application_focused_receiver_timeout": timeoutObject(ownReceiverRead.timeout),
            "application_focused_receiver_cf_type_id": ownReceiverRead.cfTypeID as Any? ?? NSNull(),
            "application_focused_window_api_call_attempted": ownFocusedWindowRead.attempted,
            "application_focused_window_error": ownFocusedWindowRead.errorCode as Any? ?? NSNull(),
            "application_focused_window_returned_late": ownFocusedWindowRead.returnedLate,
            "application_focused_window_timeout": timeoutObject(ownFocusedWindowRead.timeout),
            "application_focused_window_cf_type_id": ownFocusedWindowRead.cfTypeID as Any? ?? NSNull(),
            "system_receiver_same_cf_as_application_receiver": systemReceiverSameOwn,
            "receiver_role": receiverRole as Any? ?? NSNull(),
            "receiver_role_read": receiverRoleRead as Any? ?? NSNull(),
            "receiver_is_text_area": receiverRole == kAXTextAreaRole as String,
            "cg_window_count": cg?.count ?? 0, "cg_window_id": cgID as Any? ?? NSNull(),
            "cg_window_title_matches_owned_document": cgTitleMatch,
            "ax_window_count": windows.count,
            "ax_windows_api_call_attempted": windowsRead.attempted,
            "ax_windows_error": windowsRead.errorCode as Any? ?? NSNull(),
            "ax_windows_returned_late": windowsRead.returnedLate,
            "ax_windows_timeout": timeoutObject(windowsRead.timeout),
            "ax_windows_cf_type_id": windowsRead.cfTypeID as Any? ?? NSNull(),
            "ax_windows_array_conversion_succeeded": windowsRead.arrayConversionSucceeded,
            "ax_windows_raw_element_count": windowsRead.rawElementCount as Any? ?? NSNull(),
            "ax_windows_returned_element_count": windowsRead.returnedElementCount,
            "ax_windows_truncated": windowsRead.truncated,
            "window_in_app_count": windowInAppCount,
            "ax_window_title_matches_owned_document": titleMatch,
            "ax_window_title_read": windowTitleRead as Any? ?? NSNull(),
            "cg_ax_window_frames_match": boundsMatch, "receiver_is_in_unique_window_text_area_list": receiverInTreeCount == 1 && textAreas.count == 1,
            "owned_window_tree_scan_node_count": treeScan?.visitedCount ?? 0,
            "owned_window_tree_scan_complete": treeScan?.complete ?? false,
            "owned_window_tree_child_read_errors": treeScan?.childReadErrors ?? [],
            "owned_window_tree_children_no_value_leaf_reads": treeScan?.childNoValueLeafReads ?? [],
            "owned_window_tree_children_array_reads": treeScan?.childArrayReads ?? [],
            "owned_window_tree_child_reads_skipped_at_deadline": treeScan?.childReadsSkippedAtDeadline ?? 0,
            "owned_window_tree_role_read_failures": treeScan?.roleReadFailures ?? [],
            "receiver_window_relation_same_cf": relationReachesWindow,
            "receiver_ancestor_memberships_verified": chainMembershipsVerified,
            "receiver_ancestor_cf_relations": chainRelations,
            "ax_attribute_reads": attributes,
            "scope_verified": verified,
            "receiver_same_cf_after": false
        ]
        return ScopeRead(facts: facts, verified: verified, application: app, systemApplication: systemApplication,
                         window: window, receiver: systemReceiver, chain: chain, cgWindowID: cgID,
                         cgWindowTitle: cgWindowTitle, cgWindowFrame: cgWindowFrame, axWindowFrame: axWindowFrame)
    }

    @MainActor
    private static func attributeFacts(_ element: AXUIElement, path: String,
                                       request: CoverageRequest) -> [[String: Any]] {
        let names = [kAXRoleAttribute, kAXSubroleAttribute, kAXTitleAttribute, kAXDescriptionAttribute,
                     kAXEnabledAttribute, kAXFocusedAttribute]
        var facts = names.map { rawAttribute(element, $0 as String, path: path, request: request) }
        if path == "receiver" { facts.append(markedRange(element)) }
        return facts
    }

    @MainActor
    private static func rawAttribute(_ element: AXUIElement, _ name: String,
                                     path: String = "", request: CoverageRequest? = nil) -> [String: Any] {
        let query = copyAXAttribute(element, name)
        var result: [String: Any] = ["name": name, "api_call_attempted": query.attempted,
                                     "error_code": query.errorCode as Any? ?? NSNull(),
                                     "classification": query.errorCode.map(classify) ?? (query.timeout.attempted ? "timeout_setup_failed" : "deadline_not_attempted"),
                                     "returned_late": query.returnedLate,
                                     "timeout_setup": timeoutObject(query.timeout),
                                     "present": false]
        guard query.attempted, !query.returnedLate, query.errorCode == AXError.success.rawValue,
              let raw = query.value else { return result }
        result["present"] = true
        result["cf_type_id"] = CFGetTypeID(raw)
        if CFGetTypeID(raw) == AXUIElementGetTypeID() { result["value_kind"] = "AXUIElement" }
        if name == kAXRoleAttribute as String || name == kAXSubroleAttribute as String {
            if let value = raw as? String, value.utf8.count <= 128 { result["string_value"] = value }
        } else if name == kAXTitleAttribute as String, let request {
            if let value = raw as? String {
                result["owned_document_name_matches"] = value.contains(URL(fileURLWithPath: request.documentPath).lastPathComponent)
            }
        } else if CFGetTypeID(raw) == CFBooleanGetTypeID() {
            result["strict_cf_boolean"] = (raw as! NSNumber).boolValue
        }
        return result
    }

    @MainActor
    private static func markedRange(_ element: AXUIElement) -> [String: Any] {
        let name = NSAccessibility.Attribute.textInputMarkedRangeAttribute.rawValue
        let query = copyAXAttribute(element, name)
        var result: [String: Any] = ["name": name, "api_call_attempted": query.attempted,
                                     "error_code": query.errorCode as Any? ?? NSNull(),
                                     "classification": query.errorCode.map(classify) ?? (query.timeout.attempted ? "timeout_setup_failed" : "deadline_not_attempted"),
                                     "returned_late": query.returnedLate,
                                     "timeout_setup": timeoutObject(query.timeout),
                                     "present": false,
                                     "range_conversion_attempted": false, "range_conversion_succeeded": false]
        guard query.attempted, !query.returnedLate, query.errorCode == AXError.success.rawValue,
              let raw = query.value else { return result }
        result["present"] = true
        result["cf_type_id"] = CFGetTypeID(raw)
        guard CFGetTypeID(raw) == AXValueGetTypeID() else { return result }
        let value = unsafeDowncast(raw, to: AXValue.self)
        result["ax_value_type_raw"] = AXValueGetType(value).rawValue
        guard AXValueGetType(value) == .cfRange else { return result }
        result["range_conversion_attempted"] = true
        var range = CFRange(location: 0, length: 0)
        let converted = AXValueGetValue(value, .cfRange, &range)
        result["range_conversion_succeeded"] = converted
        if converted {
            result["location_decimal_string"] = String(range.location)
            result["length_decimal_string"] = String(range.length)
        }
        return result
    }

    @MainActor
    private static func rawSettable(_ element: AXUIElement, _ name: String) -> [String: Any] {
        let timeout = configureMessagingTimeout(element)
        guard canCallAXAfterTimeoutSetup(timeout: timeout, now: uptimeNow(), deadline: deadline) else {
            return ["name": name, "api_call_attempted": false, "error_code": NSNull(),
                    "classification": timeout.attempted ? "timeout_setup_failed_or_budget_exhausted" : "deadline_not_attempted",
                    "returned_late": false, "timeout_setup": timeoutObject(timeout), "is_settable": NSNull()]
        }
        var settable = DarwinBoolean(false)
        let error = AXUIElementIsAttributeSettable(element, name as CFString, &settable)
        let returnedLate = uptimeNow() >= deadline
        return ["name": name, "api_call_attempted": true, "error_code": error.rawValue,
                "classification": classify(error.rawValue), "returned_late": returnedLate,
                "timeout_setup": timeoutObject(timeout),
                "is_settable": error == .success && !returnedLate ? settable.boolValue as Any : NSNull()]
    }

    @MainActor
    private static func registerObservers(pid: Int32, application: AXUIElement, window: AXUIElement,
                                          chain: [RawAXNode]) -> [String: Any] {
        guard uptimeNow() < deadline else {
            return ["observer_create_api_call_attempted": false, "observer_create_error_code": NSNull(),
                    "observer_create_classification": "deadline_not_attempted",
                    "observer_create_returned_late": false,
                    "runloop_source_installed": false, "registrations": []]
        }
        var observer: AXObserver?
        let createError = AXObserverCreate(pid_t(pid), noopObserverCallback, &observer)
        let createReturnedLate = uptimeNow() >= deadline
        guard createError == .success, !createReturnedLate, let observer else {
            return ["observer_create_api_call_attempted": true, "observer_create_error_code": createError.rawValue,
                    "observer_create_classification": classify(createError.rawValue),
                    "observer_create_returned_late": createReturnedLate,
                    "runloop_source_installed": false, "registrations": []]
        }
        let source = AXObserverGetRunLoopSource(observer)
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .defaultMode)
        defer { CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, .defaultMode) }

        var registrations = [[String: Any]]()
        let appRequired: [(String, String)] = [
            ("focused_window_changed", kAXFocusedWindowChangedNotification),
            ("focused_ui_element_changed", kAXFocusedUIElementChangedNotification),
            ("window_created", kAXWindowCreatedNotification),
            ("application_activated", kAXApplicationActivatedNotification),
            ("application_deactivated", kAXApplicationDeactivatedNotification)
        ]
        for (label, name) in appRequired { registrations.append(register(observer, application, name, scope: "application", label: label)) }

        let windowRequired: [(String, String)] = [
            ("element_destroyed", kAXUIElementDestroyedNotification),
            ("window_moved", kAXWindowMovedNotification),
            ("window_resized", kAXWindowResizedNotification)
        ]
        for (label, name) in windowRequired { registrations.append(register(observer, window, name, scope: "window", label: label)) }

        let ordered = chain.filter { $0.path != "window" }
        for node in ordered {
            registrations.append(register(observer, node.element, kAXUIElementDestroyedNotification,
                                          scope: node.path, label: "element_destroyed"))
        }
        return ["observer_create_api_call_attempted": true, "observer_create_error_code": createError.rawValue,
                "observer_create_classification": classify(createError.rawValue),
                "observer_create_returned_late": false,
                "runloop_source_installed": true,
                "application_required_notifications": appRequired.map { $0.0 },
                "window_notifications": windowRequired.map { $0.0 },
                "receiver_and_nonwindow_ancestor_destroyed_count": ordered.count,
                "registrations": registrations,
                "callback_side_effects": "none"]
    }

    @MainActor
    private static func register(_ observer: AXObserver, _ element: AXUIElement,
                                 _ name: String, scope: String, label: String) -> [String: Any] {
        let timeout = configureMessagingTimeout(element)
        guard canCallAXAfterTimeoutSetup(timeout: timeout, now: uptimeNow(), deadline: deadline) else {
            return ["scope": scope, "notification": label, "api_name": name,
                    "api_call_attempted": false, "error_code": NSNull(),
                    "classification": timeout.attempted ? "timeout_setup_failed_or_budget_exhausted" : "deadline_not_attempted",
                    "returned_late": false, "timeout_setup": timeoutObject(timeout), "returned_success": false]
        }
        let error = AXObserverAddNotification(observer, element, name as CFString, nil)
        let returnedLate = uptimeNow() >= deadline
        return ["scope": scope, "notification": label, "api_name": name,
                "api_call_attempted": true, "error_code": error.rawValue, "classification": classify(error.rawValue),
                "returned_late": returnedLate, "timeout_setup": timeoutObject(timeout),
                "returned_success": error == .success && !returnedLate]
    }

    @MainActor
    private static func descendants(role expectedRole: String, of root: AXUIElement,
                                    limit: Int, depthLimit: Int) -> DescendantScan {
        var result = [AXUIElement]()
        var visited = [AXUIElement]()
        var complete = true
        var childReadErrors = [Int32]()
        var childNoValueLeafReads = [[String: Any]]()
        var childArrayReads = [[String: Any]]()
        var childReadsSkippedAtDeadline = 0
        var roleReadFailures = [[String: Any]]()
        func visit(_ element: AXUIElement, depth: Int) {
            guard uptimeNow() < deadline else { complete = false; return }
            guard visited.count < limit, depth <= depthLimit,
                  !visited.contains(where: { CFEqual($0, element) }) else { complete = false; return }
            visited.append(element)
            guard elementPID(element) == currentTargetPID else { complete = false; return }
            let roleRead = rawAttribute(element, kAXRoleAttribute as String)
            guard roleRead["api_call_attempted"] as? Bool == true,
                  roleRead["error_code"] as? Int32 == AXError.success.rawValue,
                  roleRead["returned_late"] as? Bool == false,
                  let role = roleRead["string_value"] as? String else {
                roleReadFailures.append(["node_index": visited.count - 1, "role_read": roleRead])
                complete = false
                return
            }
            if role == expectedRole {
                result.append(element)
            }
            let children = relatedElements(element, kAXChildrenAttribute as String)
            var arrayRead = arrayRelationObject(children)
            arrayRead["node_index"] = visited.count - 1
            arrayRead["attribute"] = kAXChildrenAttribute as String
            childArrayReads.append(arrayRead)
            if optionalChildrenNoValueIsLeaf(children) {
                childNoValueLeafReads.append([
                    "node_index": visited.count - 1,
                    "attribute": kAXChildrenAttribute as String,
                    "api_call_attempted": children.attempted,
                    "error_code": children.errorCode as Any? ?? NSNull(),
                    "classification": "no_value_leaf",
                    "returned_late": children.returnedLate,
                    "timeout_setup": timeoutObject(children.timeout)
                ])
                return
            }
            guard verifiedAXRead(children) else {
                if !children.attempted {
                    childReadsSkippedAtDeadline += 1
                } else if let errorCode = children.errorCode, errorCode != AXError.success.rawValue {
                    childReadErrors.append(errorCode)
                }
                complete = false
                return
            }
            for child in children.elements { visit(child, depth: depth + 1) }
        }
        visit(root, depth: 0)
        return DescendantScan(textAreas: result, visitedCount: visited.count,
                              complete: complete, childReadErrors: childReadErrors,
                              childNoValueLeafReads: childNoValueLeafReads,
                              childArrayReads: childArrayReads,
                              childReadsSkippedAtDeadline: childReadsSkippedAtDeadline,
                              roleReadFailures: roleReadFailures)
    }

    @MainActor private static var currentTargetPID: Int32 = 0

    @MainActor
    private static func copyAXAttribute(_ element: AXUIElement, _ name: String) -> AXValueRead {
        let timeout = configureMessagingTimeout(element)
        guard canCallAXAfterTimeoutSetup(timeout: timeout, now: uptimeNow(), deadline: deadline) else {
            return AXValueRead(value: nil, errorCode: nil, attempted: false,
                               returnedLate: false, timeout: timeout)
        }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        let returnedLate = uptimeNow() >= deadline
        return AXValueRead(value: value, errorCode: error.rawValue, attempted: true,
                           returnedLate: returnedLate, timeout: timeout)
    }

    @MainActor
    private static func relatedElements(_ element: AXUIElement, _ name: String) -> AXElementsRelationRead {
        let query = copyAXAttribute(element, name)
        let typeID = query.value.map(CFGetTypeID)
        guard query.attempted, query.errorCode == AXError.success.rawValue,
              !query.returnedLate, query.timeout.permitsNextCall,
              let raw = query.value, CFGetTypeID(raw) == CFArrayGetTypeID() else {
            return AXElementsRelationRead(elements: [], errorCode: query.errorCode, cfTypeID: typeID,
                                          arrayConversionSucceeded: false, rawElementCount: nil,
                                          returnedElementCount: 0, truncated: false,
                                          attempted: query.attempted, returnedLate: query.returnedLate,
                                          timeout: query.timeout)
        }
        let rawArray = unsafeDowncast(raw, to: CFArray.self)
        let rawElementCount = CFArrayGetCount(rawArray)
        guard let values = raw as? [AXUIElement], values.count == rawElementCount else {
            return AXElementsRelationRead(elements: [], errorCode: query.errorCode, cfTypeID: typeID,
                                          arrayConversionSucceeded: false, rawElementCount: rawElementCount,
                                          returnedElementCount: 0, truncated: false,
                                          attempted: query.attempted, returnedLate: false,
                                          timeout: query.timeout)
        }
        let boundedValues = Array(values.prefix(64))
        return AXElementsRelationRead(elements: boundedValues, errorCode: query.errorCode,
                                      cfTypeID: typeID,
                                      arrayConversionSucceeded: true, rawElementCount: rawElementCount,
                                      returnedElementCount: boundedValues.count,
                                      truncated: boundedValues.count < rawElementCount,
                                      attempted: true, returnedLate: false,
                                      timeout: query.timeout)
    }

    @MainActor
    private static func relatedElement(_ element: AXUIElement, _ name: String) -> AXElementRelationRead {
        let query = copyAXAttribute(element, name)
        let typeID = query.value.map(CFGetTypeID)
        guard query.attempted, query.errorCode == AXError.success.rawValue,
              !query.returnedLate, query.timeout.permitsNextCall,
              let raw = query.value, CFGetTypeID(raw) == AXUIElementGetTypeID() else {
            return AXElementRelationRead(element: nil, errorCode: query.errorCode,
                                         cfTypeID: typeID, attempted: query.attempted,
                                         returnedLate: query.returnedLate, timeout: query.timeout)
        }
        return AXElementRelationRead(element: unsafeDowncast(raw, to: AXUIElement.self),
                                     errorCode: query.errorCode, cfTypeID: typeID, attempted: true,
                                     returnedLate: false, timeout: query.timeout)
    }

    @MainActor
    private static func relatedValue(_ element: AXUIElement, _ name: String) -> AXValueRead {
        copyAXAttribute(element, name)
    }

    @MainActor
    private static func elementPID(_ element: AXUIElement) -> Int32? {
        var pid: pid_t = 0
        guard AXUIElementGetPid(element, &pid) == .success, pid > 0 else { return nil }
        return Int32(pid)
    }

    @MainActor
    private static func frame(_ element: AXUIElement) -> CGRect? {
        let position = relatedValue(element, kAXPositionAttribute as String)
        let size = relatedValue(element, kAXSizeAttribute as String)
        guard position.attempted, size.attempted,
              !position.returnedLate, !size.returnedLate,
              position.timeout.permitsNextCall, size.timeout.permitsNextCall,
              position.errorCode == AXError.success.rawValue, size.errorCode == AXError.success.rawValue,
              let position = position.value, let size = size.value,
              CFGetTypeID(position) == AXValueGetTypeID(), CFGetTypeID(size) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        var dimensions = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(position, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(size, to: AXValue.self), .cgSize, &dimensions) else { return nil }
        let rect = CGRect(origin: point, size: dimensions)
        return rect.origin.x.isFinite && rect.origin.y.isFinite && rect.width.isFinite && rect.height.isFinite
            && rect.width > 0 && rect.height > 0 ? rect : nil
    }

    private static func rectObject(_ rect: CGRect) -> [String: Double] {
        ["x": rect.origin.x, "y": rect.origin.y, "width": rect.width, "height": rect.height]
    }

    private static func sameFrame(_ first: CGRect?, _ second: CGRect?) -> Bool {
        guard let first, let second else { return false }
        return abs(first.origin.x - second.origin.x) <= 0.75 && abs(first.origin.y - second.origin.y) <= 0.75
            && abs(first.width - second.width) <= 0.75 && abs(first.height - second.height) <= 0.75
    }

    private static func cgWindows(pid: Int32) -> [[String: Any]]? {
        guard let items = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else { return nil }
        return items.filter {
            ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid
                && ($0[kCGWindowLayer as String] as? NSNumber)?.intValue == 0
                && ($0[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue == true
        }
    }

    private static func cgID(_ item: [String: Any]) -> UInt32? {
        (item[kCGWindowNumber as String] as? NSNumber)?.uint32Value
    }

    private static func cgTitle(_ item: [String: Any]) -> String? {
        item[kCGWindowName as String] as? String
    }

    private static func cgBounds(_ item: [String: Any]) -> CGRect? {
        guard let raw = item[kCGWindowBounds as String] as? NSDictionary else { return nil }
        var result = CGRect.zero
        return CGRectMakeWithDictionaryRepresentation(raw, &result) ? result : nil
    }

    @MainActor
    private static func titleMatches(_ element: AXUIElement, request: CoverageRequest) -> Bool {
        attributeString(element, kAXTitleAttribute as String)?.contains(URL(fileURLWithPath: request.documentPath).lastPathComponent) == true
    }

    @MainActor
    private static func attributeString(_ element: AXUIElement, _ name: String) -> String? {
        let query = copyAXAttribute(element, name)
        guard query.attempted, !query.returnedLate, query.timeout.permitsNextCall,
              query.errorCode == AXError.success.rawValue,
              let raw = query.value, let value = raw as? String, value.utf8.count <= 512 else { return nil }
        return value
    }

    private static func normalizedPath(_ path: String) -> String {
        URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
    }

    private static func pathForPID(_ pid: Int32) -> String? {
        guard pid > 0 else { return nil }
        var buffer = [CChar](repeating: 0, count: 4096)
        guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
        return normalizedPath(String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
    }

    private static func processIdentity(_ pid: Int32) -> KernelIdentity? {
        guard pid > 0 else { return nil }
        var stamp = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size,
              let executable = pathForPID(pid) else { return nil }
        let parent = Int32(stamp.pbi_ppid)
        return KernelIdentity(pid: pid, parentPID: parent,
                              startSeconds: Int64(stamp.pbi_start_tvsec),
                              startMicroseconds: Int64(stamp.pbi_start_tvusec),
                              executablePath: executable, parentExecutablePath: pathForPID(parent))
    }

    private static func identityObject(_ identity: KernelIdentity?) -> [String: Any] {
        guard let identity else { return [:] }
        return ["pid": identity.pid, "parent_pid": identity.parentPID,
                "start_seconds": identity.startSeconds, "start_microseconds": identity.startMicroseconds,
                "executable_path": identity.executablePath,
                "parent_executable_path": identity.parentExecutablePath as Any? ?? NSNull()]
    }

    private static func matchesIdentity(_ actual: KernelIdentity?, _ expected: KernelIdentity) -> Bool {
        actual == expected
    }

    private static func validReceipt(_ request: CoverageRequest) -> Bool {
        let receipt = request.launchReceipt
        return normalizedPath(request.executablePath) == normalizedPath(textEditExecutablePath)
            && normalizedPath(receipt.executablePath) == normalizedPath(textEditExecutablePath)
            && receipt.bundleIdentifier == textEditBundleID && request.bundleID == textEditBundleID
            && receipt.targetIdentity.pid == request.targetPID && receipt.launcherIdentity.pid == request.launcherPID
            && receipt.launcherID == request.launcherID && receipt.runID == request.runID
            && receipt.launcherIdentity.parentPID == request.probeParentPID
            && normalizedPath(receipt.documentPath) == normalizedPath(request.documentPath)
            && receipt.documentSHA256 == request.documentSHA256
            && !receipt.preexistingTextEditPIDs.contains(request.targetPID)
            && receipt.newPIDExcludedFromPreexistingSet && receipt.targetStartedWithinLaunchInterval
            && receipt.createsNewApplicationInstance && !receipt.addsToRecentItems
            && !receipt.activatesNewInstance && receipt.arguments == ["-ApplePersistenceIgnoreState", "YES"]
            && receipt.noInputAuthority && receipt.targetIdentity.parentPID == 1
            && normalizedPath(receipt.targetIdentity.executablePath) == normalizedPath(textEditExecutablePath)
    }

    private static func validOwnedDocument(_ request: CoverageRequest) -> Bool {
        let path = normalizedPath(request.documentPath)
        let temp = normalizedPath(NSTemporaryDirectory())
        guard URL(fileURLWithPath: path).lastPathComponent == documentName,
              path.hasPrefix(temp + "/"),
              let attributes = try? FileManager.default.attributesOfItem(atPath: path),
              (attributes[.ownerAccountID] as? NSNumber)?.intValue == Int(getuid()),
              (attributes[.posixPermissions] as? NSNumber)?.intValue == 0o600,
              let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else { return false }
        let hash = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
        return hash == request.documentSHA256 && hash == request.launchReceipt.documentSHA256
    }

    private static func activationObject(_ value: ActivationObservation?) -> Any {
        guard let value else { return NSNull() }
        return ["attempted": value.attempted, "attempt_count": value.attemptCount,
                "return_value": value.returnValue as Any? ?? NSNull(), "status": value.status,
                "window_verified": value.windowVerified, "cg_window_count": value.cgWindowCount,
                "ax_window_count": value.axWindowCount, "cg_window_id": value.cgWindowID as Any? ?? NSNull(),
                "frames_match": value.framesMatch,
                "system_focused_application_pid_before_activation": value.systemFocusedApplicationPIDBeforeActivation as Any? ?? NSNull()]
    }

    private static func uptimeNow() -> UInt64 { DispatchTime.now().uptimeNanoseconds }

    private static func emit(_ object: [String: Any]) {
        guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
              data.count <= outputLimit,
              let line = String(data: data, encoding: .utf8) else {
            print("{\"kind\":\"probe_error\",\"error\":\"output encoding failed or exceeded budget\"}")
            fflush(stdout)
            return
        }
        print(line)
        fflush(stdout)
    }
}
