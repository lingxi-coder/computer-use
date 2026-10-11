// Private Launch Services owner for one fresh TextEdit instance and temp document.
import AppKit
import CryptoKit
import Darwin
import Foundation

private let textEditBundleID = "com.apple.TextEdit"
private let textEditApplicationPath = "/System/Applications/TextEdit.app"
private let textEditExecutablePath = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"

private struct KernelIdentity: Codable, Equatable {
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

private struct ExistingApplication: Codable {
    let pid: Int32
    let bundleIdentifier: String
    let bundlePath: String?
    let launchDateMilliseconds: Int64?
    let kernelIdentity: KernelIdentity

    enum CodingKeys: String, CodingKey {
        case pid
        case bundleIdentifier = "bundle_identifier"
        case bundlePath = "bundle_path"
        case launchDateMilliseconds = "launch_date_milliseconds"
        case kernelIdentity = "kernel_identity"
    }
}

private struct LaunchReceipt: Codable {
    let version: Int
    let runID: String
    let launcherID: String
    let launcherIdentity: KernelIdentity
    let targetIdentity: KernelIdentity
    let bundleIdentifier: String
    let applicationPath: String
    let executablePath: String
    let documentPath: String
    let documentSHA256: String
    let preexistingTextEditPIDs: [Int32]
    let preexistingTextEditApplications: [ExistingApplication]
    let launchStartUnixMilliseconds: Int64
    let launchFinishUnixMilliseconds: Int64
    let newPIDExcludedFromPreexistingSet: Bool
    let targetStartedWithinLaunchInterval: Bool
    let createsNewApplicationInstance: Bool
    let addsToRecentItems: Bool
    let activatesNewInstance: Bool
    let arguments: [String]
    let targetParentIsLaunchd: Bool
    let noInputAuthority: Bool

    enum CodingKeys: String, CodingKey {
        case version
        case runID = "run_id"
        case launcherID = "launcher_id"
        case launcherIdentity = "launcher_identity"
        case targetIdentity = "target_identity"
        case bundleIdentifier = "bundle_identifier"
        case applicationPath = "application_path"
        case executablePath = "executable_path"
        case documentPath = "document_path"
        case documentSHA256 = "document_sha256"
        case preexistingTextEditPIDs = "preexisting_textedit_pids"
        case preexistingTextEditApplications = "preexisting_text_edit_applications"
        case launchStartUnixMilliseconds = "launch_start_unix_milliseconds"
        case launchFinishUnixMilliseconds = "launch_finish_unix_milliseconds"
        case newPIDExcludedFromPreexistingSet = "new_pid_excluded_from_preexisting_set"
        case targetStartedWithinLaunchInterval = "target_started_within_launch_interval"
        case createsNewApplicationInstance = "creates_new_application_instance"
        case addsToRecentItems = "adds_to_recent_items"
        case activatesNewInstance = "activates_new_instance"
        case arguments
        case targetParentIsLaunchd = "target_parent_is_launchd"
        case noInputAuthority = "no_input_authority"
    }
}

private struct LauncherCommand: Decodable {
    let op: String
    let runID: String?
    let documentPath: String?
    let documentSHA256: String?
    let targetPID: Int32?
    let targetStartSeconds: Int64?
    let targetStartMicroseconds: Int64?
    let cgWindowCount: Int?
    let axWindowCount: Int?
    let cgWindowID: UInt32?
    let cgTitle: String?
    let axTitle: String?
    let framesMatch: Bool?
    let systemFocusedApplicationPIDBeforeActivation: Int32?

    enum CodingKeys: String, CodingKey {
        case op
        case runID = "run_id"
        case documentPath = "document_path"
        case documentSHA256 = "document_sha256"
        case targetPID = "target_pid"
        case targetStartSeconds = "target_start_seconds"
        case targetStartMicroseconds = "target_start_microseconds"
        case cgWindowCount = "cg_window_count"
        case axWindowCount = "ax_window_count"
        case cgWindowID = "cg_window_id"
        case cgTitle = "cg_title"
        case axTitle = "ax_title"
        case framesMatch = "frames_match"
        case systemFocusedApplicationPIDBeforeActivation = "system_focused_application_pid_before_activation"
    }
}

private func normalizedPath(_ path: String) -> String {
    URL(fileURLWithPath: path).resolvingSymlinksInPath().standardizedFileURL.path
}

private func pathForPID(_ pid: Int32) -> String? {
    guard pid > 0 else { return nil }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return normalizedPath(String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
}

private func processIdentity(_ pid: Int32) -> KernelIdentity? {
    guard pid > 0 else { return nil }
    var stamp = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
    guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size,
          let executablePath = pathForPID(pid) else { return nil }
    let parentPID = Int32(stamp.pbi_ppid)
    return KernelIdentity(
        pid: pid,
        parentPID: parentPID,
        startSeconds: Int64(stamp.pbi_start_tvsec),
        startMicroseconds: Int64(stamp.pbi_start_tvusec),
        executablePath: executablePath,
        parentExecutablePath: pathForPID(parentPID)
    )
}

private func allProcessPIDs() -> [Int32]? {
    let initialBytes = proc_listpids(UInt32(PROC_ALL_PIDS), 0, nil, 0)
    guard initialBytes > 0 else { return nil }
    var capacity = max(4096, Int(initialBytes) / MemoryLayout<pid_t>.stride + 128)
    for _ in 0..<4 {
        var pids = [pid_t](repeating: 0, count: capacity)
        let copied = pids.withUnsafeMutableBufferPointer { buffer in
            proc_listpids(UInt32(PROC_ALL_PIDS), 0, buffer.baseAddress, Int32(buffer.count * MemoryLayout<pid_t>.stride))
        }
        guard copied > 0 else { return nil }
        if Int(copied) < capacity * MemoryLayout<pid_t>.stride {
            let count = Int(copied) / MemoryLayout<pid_t>.stride
            return Array(pids[0..<count].filter { $0 > 0 })
        }
        capacity *= 2
    }
    return nil
}

private func unixMilliseconds() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

private func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
          let line = String(data: data, encoding: .utf8) else { return }
    print(line)
    fflush(stdout)
}

private func receiptObject(_ receipt: LaunchReceipt) -> [String: Any]? {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(receipt),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
    return object
}

private func identityObject(_ identity: KernelIdentity) -> [String: Any]? {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(identity),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
    return object
}

private func parseCommand(_ line: String) -> LauncherCommand? {
    try? JSONDecoder().decode(LauncherCommand.self, from: Data(line.utf8))
}

@main
struct OwnedTextEditLauncherMain {
    @MainActor static func main() async {
        let launcherID = UUID().uuidString.lowercased()
        guard let ownIdentity = processIdentity(getpid()),
              ownIdentity.parentPID == getppid(),
              ownIdentity.executablePath.hasSuffix("/OwnedTextEditLauncher") else {
            emit(["kind": "launcher_error", "launcher_id": launcherID, "error": "launcher process identity unavailable"])
            return
        }
        emit(["kind": "ready", "launcher_id": launcherID, "launcher_identity": identityObject(ownIdentity) ?? [:]])

        var ownedApplication: NSRunningApplication?
        var ownedReceipt: LaunchReceipt?
        var activationAttempts = 0
        while let line = readLine(), let command = parseCommand(line) {
            if command.op == "quit" {
                let closeReport = closeOwned(application: ownedApplication, receipt: ownedReceipt)
                emit(["kind": "quit_result", "launcher_id": launcherID, "close": closeReport])
                return
            }
            if command.op == "activate_owned" {
                let result = activateOwned(
                    application: ownedApplication,
                    receipt: ownedReceipt,
                    command: command,
                    activationAttempts: &activationAttempts
                )
                var record = result
                record["kind"] = "activation_result"
                record["launcher_id"] = launcherID
                emit(record)
                continue
            }
            if command.op == "decode_check",
               let runID = command.runID,
               let documentPath = command.documentPath,
               let documentSHA256 = command.documentSHA256,
               documentSHA256.range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil,
               let receipt = protocolReceipt(
                    launcherID: launcherID, launcherIdentity: ownIdentity, runID: runID,
                    documentPath: documentPath, documentSHA256: documentSHA256
               ),
               let receiptJSON = receiptObject(receipt) {
                emit(["kind": "protocol_check", "accepted": true, "run_id": runID,
                      "document_sha256": documentSHA256, "receipt": receiptJSON,
                      "activation_command_fields": [
                        "target_pid": command.targetPID as Any? ?? NSNull(),
                        "target_start_seconds": command.targetStartSeconds as Any? ?? NSNull(),
                        "target_start_microseconds": command.targetStartMicroseconds as Any? ?? NSNull(),
                        "cg_window_count": command.cgWindowCount as Any? ?? NSNull(),
                        "ax_window_count": command.axWindowCount as Any? ?? NSNull(),
                        "cg_window_id": command.cgWindowID as Any? ?? NSNull(),
                        "cg_title": command.cgTitle as Any? ?? NSNull(),
                        "ax_title": command.axTitle as Any? ?? NSNull(),
                        "frames_match": command.framesMatch as Any? ?? NSNull(),
                        "system_focused_application_pid_before_activation": command.systemFocusedApplicationPIDBeforeActivation as Any? ?? NSNull()
                      ]])
                continue
            }
            guard command.op == "launch", ownedApplication == nil,
                  let runID = command.runID, runID.utf8.count <= 128,
                  let documentPath = command.documentPath,
                  let documentSHA256 = command.documentSHA256,
                  documentSHA256.range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil else {
                emit(["kind": "launch_error", "launcher_id": launcherID, "error": "invalid or repeated launch request"])
                continue
            }
            do {
                let result = try await launch(
                    launcherID: launcherID,
                    launcherIdentity: ownIdentity,
                    runID: runID,
                    documentPath: documentPath,
                    documentSHA256: documentSHA256
                )
                ownedApplication = result.application
                ownedReceipt = result.receipt
                guard let object = receiptObject(result.receipt) else { throw LauncherError.invalidReceipt }
                emit(["kind": "launch_receipt", "launcher_id": launcherID, "receipt": object])
            } catch {
                emit(["kind": "launch_error", "launcher_id": launcherID, "error": String(describing: error)])
            }
        }
        if ownedApplication != nil {
            emit(["kind": "stdin_closed", "launcher_id": launcherID,
                  "close": closeOwned(application: ownedApplication, receipt: ownedReceipt)])
        }
    }

    private static func protocolReceipt(
        launcherID: String,
        launcherIdentity: KernelIdentity,
        runID: String,
        documentPath: String,
        documentSHA256: String
    ) -> LaunchReceipt? {
        let target = KernelIdentity(
            pid: 12345,
            parentPID: 1,
            startSeconds: 1_791_000_000,
            startMicroseconds: 123_456,
            executablePath: normalizedPath(textEditExecutablePath),
            parentExecutablePath: "/sbin/launchd"
        )
        return LaunchReceipt(
            version: 1,
            runID: runID,
            launcherID: launcherID,
            launcherIdentity: launcherIdentity,
            targetIdentity: target,
            bundleIdentifier: textEditBundleID,
            applicationPath: normalizedPath(textEditApplicationPath),
            executablePath: normalizedPath(textEditExecutablePath),
            documentPath: documentPath,
            documentSHA256: documentSHA256,
            preexistingTextEditPIDs: [54321],
            preexistingTextEditApplications: [],
            launchStartUnixMilliseconds: 1_791_000_000_000,
            launchFinishUnixMilliseconds: 1_791_000_001_000,
            newPIDExcludedFromPreexistingSet: true,
            targetStartedWithinLaunchInterval: true,
            createsNewApplicationInstance: true,
            addsToRecentItems: false,
            activatesNewInstance: false,
            arguments: ["-ApplePersistenceIgnoreState", "YES"],
            targetParentIsLaunchd: true,
            noInputAuthority: true
        )
    }

    private static func launch(
        launcherID: String,
        launcherIdentity: KernelIdentity,
        runID: String,
        documentPath: String,
        documentSHA256: String
    ) async throws -> (application: NSRunningApplication, receipt: LaunchReceipt) {
        let applicationURL = URL(fileURLWithPath: textEditApplicationPath)
        let executablePath = normalizedPath(textEditExecutablePath)
        let documentURL = URL(fileURLWithPath: documentPath).standardizedFileURL
        let resolvedDocumentPath = normalizedPath(documentURL.path)
        let resolvedTempDirectory = normalizedPath(NSTemporaryDirectory())
        let documentAttributes = try? FileManager.default.attributesOfItem(atPath: documentURL.path)
        let documentMode = (documentAttributes?[.posixPermissions] as? NSNumber)?.intValue
        let documentOwner = (documentAttributes?[.ownerAccountID] as? NSNumber)?.intValue
        let documentSize = (documentAttributes?[.size] as? NSNumber)?.intValue
        guard documentURL.lastPathComponent == "owned-unicode-fixture.txt",
              FileManager.default.fileExists(atPath: documentURL.path),
              resolvedDocumentPath.hasPrefix(resolvedTempDirectory + "/"),
              documentMode.map({ $0 & 0o777 }) == 0o600,
              documentOwner == Int(getuid()),
              let documentSize, documentSize <= 4096,
              let documentData = try? Data(contentsOf: documentURL),
              SHA256.hash(data: documentData).map({ String(format: "%02x", $0) }).joined() == documentSHA256,
              FileManager.default.fileExists(atPath: executablePath) else { throw LauncherError.inputOrAppUnavailable }

        let beforeApplications = NSWorkspace.shared.runningApplications.filter { $0.bundleIdentifier == textEditBundleID }
        guard let kernelPIDs = allProcessPIDs() else { throw LauncherError.processListUnavailable }
        var preexistingByPID: [Int32: ExistingApplication] = [:]
        for application in beforeApplications {
            let pid = Int32(application.processIdentifier)
            guard pid > 0, let identity = processIdentity(pid) else { throw LauncherError.existingProcessIdentityUnavailable }
            preexistingByPID[pid] = ExistingApplication(
                pid: pid,
                bundleIdentifier: application.bundleIdentifier ?? "",
                bundlePath: application.bundleURL.map { normalizedPath($0.path) },
                launchDateMilliseconds: application.launchDate.map { Int64($0.timeIntervalSince1970 * 1000) },
                kernelIdentity: identity
            )
        }
        for pid in kernelPIDs where pathForPID(pid) == executablePath && preexistingByPID[pid] == nil {
            guard let identity = processIdentity(pid) else { throw LauncherError.existingProcessIdentityUnavailable }
            preexistingByPID[pid] = ExistingApplication(
                pid: pid,
                bundleIdentifier: textEditBundleID,
                bundlePath: textEditApplicationPath,
                launchDateMilliseconds: nil,
                kernelIdentity: identity
            )
        }

        let preexisting = preexistingByPID.values.sorted { $0.pid < $1.pid }
        let start = unixMilliseconds()
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.createsNewApplicationInstance = true
        configuration.addsToRecentItems = false
        configuration.activates = false
        configuration.arguments = ["-ApplePersistenceIgnoreState", "YES"]
        let application = try await NSWorkspace.shared.open(
            [documentURL], withApplicationAt: applicationURL, configuration: configuration
        )
        let openCompleted = unixMilliseconds()
        let pid = Int32(application.processIdentifier)
        guard pid > 0, application.bundleIdentifier == textEditBundleID,
              let returnedPath = application.executableURL.map({ normalizedPath($0.path) }), returnedPath == executablePath,
              !preexistingByPID.keys.contains(pid),
              let targetIdentity = waitForIdentity(pid, executablePath: executablePath, afterMilliseconds: start, untilMilliseconds: openCompleted + 10_000) else {
            if let identity = processIdentity(pid), identity.executablePath == executablePath {
                _ = terminateIfUnchanged(application, identity: identity)
            }
            throw LauncherError.newInstanceIdentityUnavailable
        }
        let finish = unixMilliseconds()
        let began = targetIdentity.startSeconds * 1000 + targetIdentity.startMicroseconds / 1000
        let startedInInterval = began >= start - 1000 && began <= finish
        guard startedInInterval else {
            _ = terminateIfUnchanged(application, identity: targetIdentity)
            throw LauncherError.launchTimeDidNotMatch
        }
        let receipt = LaunchReceipt(
            version: 1,
            runID: runID,
            launcherID: launcherID,
            launcherIdentity: launcherIdentity,
            targetIdentity: targetIdentity,
            bundleIdentifier: textEditBundleID,
            applicationPath: normalizedPath(applicationURL.path),
            executablePath: executablePath,
            documentPath: documentURL.path,
            documentSHA256: documentSHA256,
            preexistingTextEditPIDs: preexisting.map(\.pid),
            preexistingTextEditApplications: preexisting,
            launchStartUnixMilliseconds: start,
            launchFinishUnixMilliseconds: finish,
            newPIDExcludedFromPreexistingSet: !preexistingByPID.keys.contains(pid),
            targetStartedWithinLaunchInterval: startedInInterval,
            createsNewApplicationInstance: configuration.createsNewApplicationInstance,
            addsToRecentItems: configuration.addsToRecentItems,
            activatesNewInstance: configuration.activates,
            arguments: configuration.arguments,
            targetParentIsLaunchd: targetIdentity.parentPID == 1,
            noInputAuthority: true
        )
        return (application, receipt)
    }

    private static func waitForIdentity(
        _ pid: Int32,
        executablePath: String,
        afterMilliseconds: Int64,
        untilMilliseconds: Int64
    ) -> KernelIdentity? {
        let deadline = uptimeNanoseconds() + 10_000_000_000
        while uptimeNanoseconds() < deadline {
            if let identity = processIdentity(pid), identity.executablePath == executablePath {
                let began = identity.startSeconds * 1000 + identity.startMicroseconds / 1000
                if began >= afterMilliseconds - 1000 && began <= untilMilliseconds { return identity }
                return nil
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
        return nil
    }

    private static func activateOwned(
        application: NSRunningApplication?,
        receipt: LaunchReceipt?,
        command: LauncherCommand,
        activationAttempts: inout Int
    ) -> [String: Any] {
        guard let application, let receipt else {
            return ["run_id": command.runID as Any? ?? NSNull(), "attempt_count": 0, "attempted": false,
                    "return_value": NSNull(), "status": "no_owned_instance"]
        }
        let expectedTitle = URL(fileURLWithPath: receipt.documentPath).lastPathComponent
        let readinessVerified = command.targetPID == receipt.targetIdentity.pid &&
            command.targetStartSeconds == receipt.targetIdentity.startSeconds &&
            command.targetStartMicroseconds == receipt.targetIdentity.startMicroseconds &&
            command.runID == receipt.runID &&
            command.cgWindowCount == 1 && command.axWindowCount == 1 &&
            (command.cgWindowID ?? 0) > 0 && command.framesMatch == true &&
            (command.cgTitle?.contains(expectedTitle) ?? false) &&
            (command.axTitle?.contains(expectedTitle) ?? false) &&
            !receipt.preexistingTextEditPIDs.contains(receipt.targetIdentity.pid)
        guard readinessVerified else {
            return ["run_id": command.runID as Any? ?? NSNull(), "attempt_count": activationAttempts,
                    "attempted": false, "return_value": NSNull(), "status": "window_readiness_or_receipt_mismatch",
                    "expected_document_title": expectedTitle]
        }
        guard activationAttempts == 0 else {
            return ["run_id": command.runID as Any? ?? NSNull(), "attempt_count": activationAttempts,
                    "attempted": false, "return_value": NSNull(), "status": "activation_already_attempted"]
        }
        let identity = receipt.targetIdentity
        guard processIdentity(identity.pid) == identity,
              application.processIdentifier == identity.pid,
              application.bundleIdentifier == receipt.bundleIdentifier,
              application.executableURL.map({ normalizedPath($0.path) }) == receipt.executablePath,
              !application.isTerminated else {
            return ["run_id": receipt.runID, "attempt_count": 0, "attempted": false,
                    "return_value": NSNull(), "status": "owned_identity_changed_before_activation",
                    "target_identity_before_activation": identityObject(processIdentity(identity.pid) ?? identity) ?? [:]]
        }
        activationAttempts += 1
        let returned = application.activate(options: [])
        return [
            "run_id": receipt.runID,
            "attempt_count": activationAttempts,
            "attempted": true,
            "return_value": returned,
            "status": "activation_api_returned",
            "target_identity_before_activation": identityObject(identity) ?? [:],
            "preexisting_textedit_pids": receipt.preexistingTextEditPIDs,
            "window_readiness": [
                "verified": readinessVerified,
                "cg_window_count": command.cgWindowCount as Any? ?? NSNull(),
                "ax_window_count": command.axWindowCount as Any? ?? NSNull(),
                "cg_window_id": command.cgWindowID as Any? ?? NSNull(),
                "cg_title": command.cgTitle as Any? ?? NSNull(),
                "ax_title": command.axTitle as Any? ?? NSNull(),
                "frames_match": command.framesMatch as Any? ?? NSNull()
            ],
            "system_focused_application_pid_before_activation": command.systemFocusedApplicationPIDBeforeActivation as Any? ?? NSNull(),
            "activation_request_accepted": true,
            "activation_return_is_not_focus_effect_proof": true
        ]
    }

    private static func terminateIfUnchanged(_ application: NSRunningApplication, identity: KernelIdentity) -> Bool {
        guard processIdentity(identity.pid) == identity,
              application.processIdentifier == identity.pid,
              application.bundleIdentifier == textEditBundleID,
              application.executableURL.map({ normalizedPath($0.path) }) == identity.executablePath else { return false }
        return application.terminate()
    }

    private static func closeOwned(application: NSRunningApplication?, receipt: LaunchReceipt?) -> [String: Any] {
        guard let application, let receipt else { return ["status": "no_owned_instance"] }
        let identity = receipt.targetIdentity
        guard processIdentity(identity.pid) == identity,
              application.processIdentifier == identity.pid,
              application.bundleIdentifier == receipt.bundleIdentifier,
              application.executableURL.map({ normalizedPath($0.path) }) == receipt.executablePath else {
            return ["status": "skipped_identity_changed", "target_pid": identity.pid]
        }
        let terminateRequested = application.terminate()
        let deadline = uptimeNanoseconds() + 5_000_000_000
        while uptimeNanoseconds() < deadline {
            guard let current = processIdentity(identity.pid) else {
                return ["status": "terminated", "terminate_requested": terminateRequested, "target_pid": identity.pid]
            }
            guard current == identity else { return ["status": "pid_identity_changed", "target_pid": identity.pid] }
            Thread.sleep(forTimeInterval: 0.05)
        }
        return ["status": "still_running_after_bounded_terminate", "terminate_requested": terminateRequested, "target_pid": identity.pid]
    }
}

private enum LauncherError: Error, CustomStringConvertible {
    case invalidReceipt
    case inputOrAppUnavailable
    case processListUnavailable
    case existingProcessIdentityUnavailable
    case newInstanceIdentityUnavailable
    case launchTimeDidNotMatch

    var description: String {
        switch self {
        case .invalidReceipt: return "could not serialize launch receipt"
        case .inputOrAppUnavailable: return "fixed TextEdit executable or owned temp document unavailable"
        case .processListUnavailable: return "bounded kernel PID snapshot unavailable"
        case .existingProcessIdentityUnavailable: return "preexisting TextEdit kernel identity unavailable"
        case .newInstanceIdentityUnavailable: return "NSWorkspace did not return a verified new TextEdit instance"
        case .launchTimeDidNotMatch: return "returned TextEdit process predates or exceeds launch interval"
        }
    }
}

private func uptimeNanoseconds() -> UInt64 { DispatchTime.now().uptimeNanoseconds }
