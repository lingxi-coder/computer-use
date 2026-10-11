import AppKit
import CryptoKit
import Darwin
import Foundation

private let textEditBundleID = "com.apple.TextEdit"
private let textEditApplicationPath = "/System/Applications/TextEdit.app"
private let textEditExecutablePath = "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit"
private let maximumDocumentBytes = 4096

private struct ProcessIdentity: Codable, Equatable {
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
    let kernelIdentity: ProcessIdentity

    enum CodingKeys: String, CodingKey {
        case pid
        case bundleIdentifier = "bundle_identifier"
        case bundlePath = "bundle_path"
        case kernelIdentity = "kernel_identity"
    }
}

private struct LaunchReceipt: Codable {
    let version: Int
    let runID: String
    let launcherID: String
    let launcherIdentity: ProcessIdentity
    let targetIdentity: ProcessIdentity
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

private struct LaunchCommand {
    let runID: String
    let documentPath: String
    let documentSHA256: String
}

private func normalizedPath(_ path: String) -> String {
    // Match Node fs.realpathSync. Foundation preserves macOS /var aliases,
    // while POSIX realpath resolves them to /private/var.
    if let canonical = path.withCString({ realpath($0, nil) }) {
        defer { free(canonical) }
        return String(cString: canonical)
    }
    return URL(fileURLWithPath: path).standardizedFileURL.path
}

private func pathForPID(_ pid: Int32) -> String? {
    guard pid > 0 else { return nil }
    var buffer = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { return nil }
    return normalizedPath(String(decoding: buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
}

private func processIdentity(_ pid: Int32) -> ProcessIdentity? {
    guard pid > 0 else { return nil }
    var stamp = proc_bsdinfo()
    let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
    guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &stamp, size) == size,
          let executablePath = pathForPID(pid) else { return nil }
    let parentPID = Int32(stamp.pbi_ppid)
    return ProcessIdentity(
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
            return Array(pids[0..<(Int(copied) / MemoryLayout<pid_t>.stride)].filter { $0 > 0 })
        }
        capacity *= 2
    }
    return nil
}

private func unixMilliseconds() -> Int64 { Int64(Date().timeIntervalSince1970 * 1000) }

@MainActor
private final class OwnedInstance {
    let application: NSRunningApplication
    let identity: ProcessIdentity
    let runID: String

    init(application: NSRunningApplication, identity: ProcessIdentity, runID: String) {
        self.application = application
        self.identity = identity
        self.runID = runID
    }
}

@MainActor
private enum OwnedDocumentLauncherCore {
    fileprivate static func launch(
        launcherID: String,
        launcherIdentity: ProcessIdentity,
        runID: String,
        documentPath: String,
        documentSHA256: String
    ) async throws -> (instance: OwnedInstance, receipt: LaunchReceipt) {
        guard UUID(uuidString: runID)?.uuidString.lowercased() == runID,
              documentSHA256.range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil else {
            throw LauncherError.invalidCommand
        }
        let applicationURL = URL(fileURLWithPath: textEditApplicationPath)
        let executablePath = normalizedPath(textEditExecutablePath)
        let documentURL = URL(fileURLWithPath: documentPath).standardizedFileURL
        let resolvedDocumentPath = normalizedPath(documentURL.path)
        let resolvedTemporaryDirectory = normalizedPath(NSTemporaryDirectory())
        let attributes = try? FileManager.default.attributesOfItem(atPath: documentURL.path)
        let mode = (attributes?[.posixPermissions] as? NSNumber)?.intValue
        let owner = (attributes?[.ownerAccountID] as? NSNumber)?.intValue
        let size = (attributes?[.size] as? NSNumber)?.intValue
        guard documentURL.lastPathComponent == "document.txt",
              resolvedDocumentPath == documentPath,
              resolvedDocumentPath.hasPrefix(resolvedTemporaryDirectory + "/"),
              FileManager.default.fileExists(atPath: documentURL.path),
              mode.map({ $0 & 0o777 }) == 0o600,
              owner == Int(getuid()),
              let size, size <= maximumDocumentBytes,
              let bytes = try? Data(contentsOf: documentURL),
              SHA256.hash(data: bytes).map({ String(format: "%02x", $0) }).joined() == documentSHA256,
              FileManager.default.fileExists(atPath: executablePath) else {
            throw LauncherError.invalidDocument
        }

        let beforeApplications = NSWorkspace.shared.runningApplications.filter { $0.bundleIdentifier == textEditBundleID }
        guard let kernelPIDs = allProcessPIDs() else { throw LauncherError.processListUnavailable }
        var preexistingByPID: [Int32: ExistingApplication] = [:]
        for application in beforeApplications {
            let pid = Int32(application.processIdentifier)
            guard pid > 0 else { throw LauncherError.existingProcessIdentityUnavailable }
            guard let identity = processIdentity(pid) else {
                // NSWorkspace may briefly retain an entry after a confirmed
                // exit. Only the kernel's definite absence removes that entry
                // from the preexisting set; permission errors remain unknown.
                let probe = kill(pid, 0)
                let probeError = errno
                if workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: probe,
                    probeError: probeError, secondIdentityAbsent: processIdentity(pid) == nil) { continue }
                throw LauncherError.existingProcessIdentityUnavailable
            }
            preexistingByPID[pid] = ExistingApplication(
                pid: pid,
                bundleIdentifier: application.bundleIdentifier ?? "",
                bundlePath: application.bundleURL.map { normalizedPath($0.path) },
                kernelIdentity: identity
            )
        }
        for pid in kernelPIDs where pathForPID(pid) == executablePath && preexistingByPID[pid] == nil {
            guard let identity = processIdentity(pid) else { throw LauncherError.existingProcessIdentityUnavailable }
            preexistingByPID[pid] = ExistingApplication(
                pid: pid,
                bundleIdentifier: textEditBundleID,
                bundlePath: textEditApplicationPath,
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
        let application: NSRunningApplication
        do {
            application = try await NSWorkspace.shared.open([documentURL], withApplicationAt: applicationURL, configuration: configuration)
        } catch {
            throw LauncherError.launchUnavailable
        }
        let openCompleted = unixMilliseconds()
        let pid = Int32(application.processIdentifier)
        guard pid > 0,
              application.bundleIdentifier == textEditBundleID,
              application.executableURL.map({ normalizedPath($0.path) }) == executablePath,
              !preexistingByPID.keys.contains(pid),
              let identity = waitForIdentity(pid, executablePath: executablePath, after: start, until: openCompleted + 10_000) else {
            throw LauncherError.identityUnavailableAfterLaunch
        }
        let finish = unixMilliseconds()
        let began = identity.startSeconds * 1000 + identity.startMicroseconds / 1000
        let inInterval = began >= start - 1000 && began <= finish
        guard inInterval else {
            let instance = OwnedInstance(application: application, identity: identity, runID: runID)
            _ = stopOwned(instance: instance, receipt: nil, runID: runID)
            throw LauncherError.launchIntervalMismatch
        }
        let receipt = LaunchReceipt(
            version: 1,
            runID: runID,
            launcherID: launcherID,
            launcherIdentity: launcherIdentity,
            targetIdentity: identity,
            bundleIdentifier: textEditBundleID,
            applicationPath: normalizedPath(applicationURL.path),
            executablePath: executablePath,
            documentPath: resolvedDocumentPath,
            documentSHA256: documentSHA256,
            preexistingTextEditPIDs: preexisting.map(\.pid),
            preexistingTextEditApplications: preexisting,
            launchStartUnixMilliseconds: start,
            launchFinishUnixMilliseconds: finish,
            newPIDExcludedFromPreexistingSet: !preexistingByPID.keys.contains(pid),
            targetStartedWithinLaunchInterval: inInterval,
            createsNewApplicationInstance: configuration.createsNewApplicationInstance,
            addsToRecentItems: configuration.addsToRecentItems,
            activatesNewInstance: configuration.activates,
            arguments: configuration.arguments,
            targetParentIsLaunchd: identity.parentPID == 1,
            noInputAuthority: true
        )
        return (OwnedInstance(application: application, identity: identity, runID: runID), receipt)
    }

    fileprivate static func stopOwned(instance: OwnedInstance?, receipt: LaunchReceipt?, runID: String) -> (status: String, pid: Int32?) {
        guard let instance, let receipt, instance.runID == runID, receipt.runID == runID else {
            return ("no_owned_instance", nil)
        }
        let identity = instance.identity
        guard instance.application.processIdentifier == identity.pid,
              instance.application.bundleIdentifier == textEditBundleID,
              instance.application.executableURL.map({ normalizedPath($0.path) }) == identity.executablePath,
              identity.executablePath == normalizedPath(textEditExecutablePath) else {
            return ("unknown", identity.pid)
        }
        if let current = processIdentity(identity.pid), current != identity {
            return ("owned_identity_gone", identity.pid)
        }
        if processIdentity(identity.pid) == nil {
            if kill(identity.pid, 0) == -1 && errno == ESRCH { return ("terminated", identity.pid) }
            return ("unknown", identity.pid)
        }
        guard processIdentity(identity.pid) == identity else { return ("unknown", identity.pid) }
        _ = instance.application.terminate()
        let deadline = DispatchTime.now().uptimeNanoseconds + 5_000_000_000
        while DispatchTime.now().uptimeNanoseconds < deadline {
            if let current = processIdentity(identity.pid) {
                if current != identity { return ("owned_identity_gone", identity.pid) }
            } else if kill(identity.pid, 0) == -1 && errno == ESRCH {
                return ("terminated", identity.pid)
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
        return ("still_running", identity.pid)
    }

    private static func waitForIdentity(_ pid: Int32, executablePath: String, after: Int64, until: Int64) -> ProcessIdentity? {
        let deadline = DispatchTime.now().uptimeNanoseconds + 10_000_000_000
        while DispatchTime.now().uptimeNanoseconds < deadline {
            if let identity = processIdentity(pid), identity.executablePath == executablePath {
                let began = identity.startSeconds * 1000 + identity.startMicroseconds / 1000
                if began >= after - 1000 && began <= until { return identity }
                return nil
            }
            Thread.sleep(forTimeInterval: 0.05)
        }
        return nil
    }
}

private func workspaceProcessIsDefinitelyGone(identityAbsent: Bool, probe: Int32,
    probeError: Int32, secondIdentityAbsent: Bool) -> Bool {
    identityAbsent && probe == -1 && probeError == ESRCH && secondIdentityAbsent
}

private enum Command {
    case launch(LaunchCommand)
    case stop(runID: String)
    case activate(runID: String)
    case quit
}

private func parseCommand(_ line: String) -> Command? {
    guard line.utf8.count <= 8192,
          let data = line.data(using: .utf8),
          let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let op = object["op"] as? String else { return nil }
    switch op {
    case "launch":
        guard Set(object.keys) == Set(["op", "run_id", "document_path", "document_sha256"]),
              let runID = object["run_id"] as? String,
              let documentPath = object["document_path"] as? String,
              let documentSHA256 = object["document_sha256"] as? String else { return nil }
        return .launch(LaunchCommand(runID: runID, documentPath: documentPath, documentSHA256: documentSHA256))
    case "stop":
        guard Set(object.keys) == Set(["op", "run_id"]), let runID = object["run_id"] as? String else { return nil }
        return .stop(runID: runID)
    case "activate":
        guard Set(object.keys) == Set(["op", "run_id"]), let runID = object["run_id"] as? String else { return nil }
        return .activate(runID: runID)
    case "quit":
        guard Set(object.keys) == Set(["op"]) else { return nil }
        return .quit
    default:
        return nil
    }
}

private func emit(_ object: [String: Any]) {
    guard let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]),
          let line = String(data: data, encoding: .utf8) else { return }
    print(line)
    fflush(stdout)
}

private func object<T: Encodable>(_ value: T) -> [String: Any] {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys]
    guard let data = try? encoder.encode(value),
          let result = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
    return result
}

private enum LauncherError: Error, CustomStringConvertible {
    case invalidCommand
    case invalidDocument
    case processListUnavailable
    case existingProcessIdentityUnavailable
    case launchUnavailable
    case identityUnavailableAfterLaunch
    case launchIntervalMismatch

    var mayHaveLaunched: Bool {
        switch self {
        case .launchUnavailable, .identityUnavailableAfterLaunch, .launchIntervalMismatch: true
        default: false
        }
    }

    var description: String {
        switch self {
        case .invalidCommand: "invalid launch command"
        case .invalidDocument: "private document validation failed"
        case .processListUnavailable: "process identity snapshot unavailable"
        case .existingProcessIdentityUnavailable: "preexisting TextEdit identity unavailable"
        case .launchUnavailable: "TextEdit launch request failed"
        case .identityUnavailableAfterLaunch: "new TextEdit identity unavailable after launch"
        case .launchIntervalMismatch: "TextEdit start time fell outside the launch interval"
        }
    }
}

@main
struct OwnedDocumentLauncherEntry {
    @MainActor static func main() async {
        if CommandLine.arguments.dropFirst() == ["--self-test"] {
            let cases = [
                workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: -1, probeError: ESRCH, secondIdentityAbsent: true),
                !workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: 0, probeError: ESRCH, secondIdentityAbsent: true),
                !workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: -1, probeError: EPERM, secondIdentityAbsent: true),
                !workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: -1, probeError: EINVAL, secondIdentityAbsent: true),
                !workspaceProcessIsDefinitelyGone(identityAbsent: false, probe: -1, probeError: ESRCH, secondIdentityAbsent: true),
                !workspaceProcessIsDefinitelyGone(identityAbsent: true, probe: -1, probeError: ESRCH, secondIdentityAbsent: false),
            ]
            guard cases.allSatisfy({ $0 }) else { exit(2) }
            emit(["kind": "owned_launcher_self_test", "passed": cases.count]); return
        }
        let launcherID = UUID().uuidString.lowercased()
        guard let identity = processIdentity(getpid()),
              identity.parentPID == getppid(),
              identity.executablePath.hasSuffix("/cu-macos-owned-document-launcher") else {
            emit(["kind": "launcher_error", "launcher_id": launcherID, "error": "launcher identity unavailable"])
            return
        }
        emit(["kind": "ready", "launcher_id": launcherID, "launcher_identity": object(identity)])
        var ownedInstance: OwnedInstance?
        var receipt: LaunchReceipt?
        while let line = readLine() {
            guard let command = parseCommand(line) else {
                emit(["kind": "protocol_error", "launcher_id": launcherID, "error": "invalid command"])
                continue
            }
            switch command {
            case .launch(let request):
                guard ownedInstance == nil, receipt == nil else {
                    emit(["kind": "launch_error", "launcher_id": launcherID, "status": "not_launched", "error": "launch already registered"])
                    continue
                }
                do {
                    let result = try await OwnedDocumentLauncherCore.launch(
                        launcherID: launcherID, launcherIdentity: identity, runID: request.runID,
                        documentPath: request.documentPath, documentSHA256: request.documentSHA256
                    )
                    ownedInstance = result.instance
                    receipt = result.receipt
                    emit(["kind": "launch_receipt", "launcher_id": launcherID, "receipt": object(result.receipt)])
                } catch let error as LauncherError {
                    emit(["kind": "launch_error", "launcher_id": launcherID,
                          "status": error.mayHaveLaunched ? "unknown" : "not_launched", "error": error.description])
                } catch {
                    emit(["kind": "launch_error", "launcher_id": launcherID, "status": "unknown", "error": "launch outcome unknown"])
                }
            case .stop(let runID):
                let result = OwnedDocumentLauncherCore.stopOwned(instance: ownedInstance, receipt: receipt, runID: runID)
                if result.status == "terminated" || result.status == "owned_identity_gone" {
                    ownedInstance = nil
                    receipt = nil
                }
                emit(["kind": "stop_result", "launcher_id": launcherID, "run_id": runID,
                      "status": result.status, "target_pid": result.pid as Any? ?? NSNull()])
            case .activate(let runID):
                var activated = false
                if let instance = ownedInstance, let receipt,
                   instance.runID == runID, receipt.runID == runID,
                   instance.identity == receipt.targetIdentity,
                   instance.identity.executablePath == normalizedPath(textEditExecutablePath),
                   instance.application.bundleIdentifier == textEditBundleID,
                   processIdentity(instance.identity.pid) == instance.identity {
                    activated = instance.application.activate(options: [])
                        && processIdentity(instance.identity.pid) == instance.identity
                }
                emit(["kind": "activation_result", "launcher_id": launcherID, "run_id": runID,
                      "status": activated ? "activated" : "unknown",
                      "target_pid": receipt?.targetIdentity.pid as Any? ?? NSNull()])
            case .quit:
                let runID = receipt?.runID ?? ""
                let result = OwnedDocumentLauncherCore.stopOwned(instance: ownedInstance, receipt: receipt, runID: runID)
                if result.status == "terminated" || result.status == "owned_identity_gone" || result.status == "no_owned_instance" {
                    emit(["kind": "quit_result", "launcher_id": launcherID, "status": result.status])
                    return
                }
                emit(["kind": "quit_result", "launcher_id": launcherID, "status": "unknown"])
            }
        }
        let result = OwnedDocumentLauncherCore.stopOwned(instance: ownedInstance, receipt: receipt, runID: receipt?.runID ?? "")
        emit(["kind": "stdin_closed", "launcher_id": launcherID, "status": result.status])
    }
}
