import AppKit
import CryptoKit
import Darwin
import Foundation
import os

private let ownedBootstrapKeys: Set<String> = [
    "version", "registration_id", "pid", "start_seconds", "start_micros",
    "executable_path", "bundle_id", "document_path", "document_sha256",
    "document_dev", "document_ino", "directory_dev", "directory_ino", "issued_unix_ms",
]

/// This prefix is read only with an explicit private child-process argument.
/// It is never a NativeRequest field or a model/renderer message.
struct OwnedDocumentBootstrap: Decodable, Sendable {
    let version: Int
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

    static func decode(_ data: Data, nowMS: UInt64) throws -> OwnedDocumentBootstrap {
        guard !data.isEmpty, data.count <= 8192,
              let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(fields.keys) == ownedBootstrapKeys else { throw NativeFailure("invalid private owned document bootstrap") }
        let value = try JSONDecoder().decode(Self.self, from: data)
        guard value.version == 1,
              UUID(uuidString: value.registration_id)?.uuidString.lowercased() == value.registration_id,
              value.pid > 0, value.pid <= UInt32(Int32.max), value.start_seconds > 0,
              value.start_micros < 1_000_000,
              value.executable_path == "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
              value.bundle_id == "com.apple.TextEdit",
              value.document_path.hasPrefix("/"), value.document_path.utf8.count <= 4096,
              !value.document_path.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              URL(fileURLWithPath: value.document_path).lastPathComponent == "document.txt",
              value.document_sha256.range(of: "^[0-9a-f]{64}$", options: .regularExpression) != nil,
              value.document_ino > 0, value.directory_ino > 0,
              [value.document_dev, value.document_ino, value.directory_dev, value.directory_ino].allSatisfy({ $0 <= 9_007_199_254_740_991 }),
              value.issued_unix_ms > 0,
              value.issued_unix_ms <= nowMS || value.issued_unix_ms - nowMS <= 1_000,
              nowMS <= value.issued_unix_ms || nowMS - value.issued_unix_ms <= 5_000 else {
            throw NativeFailure("invalid private owned document bootstrap")
        }
        return value
    }

    static func read(from handle: FileHandle, nowMS: UInt64? = nil) throws -> OwnedDocumentBootstrap {
        func exact(_ count: Int) throws -> Data {
            var data = Data()
            while data.count < count {
                guard let part = try handle.read(upToCount: count - data.count), !part.isEmpty else {
                    throw NativeFailure("truncated private owned document bootstrap")
                }
                data.append(part)
            }
            return data
        }
        let header = try exact(4)
        let count = header.reduce(0) { ($0 << 8) | Int($1) }
        guard count > 0, count <= 8192 else { throw NativeFailure("invalid private owned document bootstrap length") }
        let payload = try exact(count)
        return try decode(payload, nowMS: nowMS ?? UInt64(Date().timeIntervalSince1970 * 1000))
    }

    func verifiedRegistration() throws -> OwnedDocumentReceiverRegistration {
        let invalid = NativeFailure("private owned document identity is unavailable")
        guard ProcessStamp.read(pid: pid) == ProcessStamp(seconds: start_seconds, micros: start_micros),
              NSRunningApplication(processIdentifier: Int32(pid))?.bundleIdentifier == bundle_id,
              ownedBootstrapExecutable(pid: pid) == executable_path,
              let canonicalDocument = ownedBootstrapCanonicalPath(document_path), canonicalDocument == document_path,
              let canonicalTemp = ownedBootstrapCanonicalPath(NSTemporaryDirectory()),
              canonicalDocument.hasPrefix(canonicalTemp + "/") else { throw invalid }
        let directory = URL(fileURLWithPath: document_path).deletingLastPathComponent().path
        var dir = stat()
        var file = stat()
        guard lstat(directory, &dir) == 0, lstat(document_path, &file) == 0,
              (dir.st_mode & S_IFMT) == S_IFDIR, (file.st_mode & S_IFMT) == S_IFREG,
              dir.st_uid == getuid(), file.st_uid == getuid(),
              (dir.st_mode & 0o777) == 0o700, (file.st_mode & 0o777) == 0o600,
              dir.st_dev >= 0, file.st_dev >= 0,
              UInt64(dir.st_dev) == directory_dev, UInt64(dir.st_ino) == directory_ino,
              UInt64(file.st_dev) == document_dev, UInt64(file.st_ino) == document_ino,
              file.st_nlink == 1, file.st_size >= 0, file.st_size <= 4096 else { throw invalid }
        let fd = open(document_path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw invalid }
        defer { close(fd) }
        var descriptor = stat()
        var finalDirectory = stat()
        guard fstat(fd, &descriptor) == 0, descriptor.st_dev == file.st_dev,
              descriptor.st_ino == file.st_ino, descriptor.st_size == file.st_size else { throw invalid }
        var bytes = [UInt8](repeating: 0, count: 4097)
        var total = 0
        while total < bytes.count {
            let copied = bytes.withUnsafeMutableBytes { buffer in
                Darwin.read(fd, buffer.baseAddress!.advanced(by: total), buffer.count - total)
            }
            guard copied >= 0 else { throw invalid }
            if copied == 0 { break }
            total += copied
        }
        guard total == file.st_size, total <= 4096,
              SHA256.hash(data: Data(bytes.prefix(total))).map({ String(format: "%02x", $0) }).joined() == document_sha256,
              fstat(fd, &descriptor) == 0, descriptor.st_dev == file.st_dev,
              descriptor.st_ino == file.st_ino, descriptor.st_size == file.st_size,
              lstat(document_path, &descriptor) == 0, descriptor.st_dev == file.st_dev, descriptor.st_ino == file.st_ino,
              (descriptor.st_mode & S_IFMT) == S_IFREG, descriptor.st_uid == getuid(),
              (descriptor.st_mode & 0o777) == 0o600, descriptor.st_nlink == 1,
              lstat(directory, &finalDirectory) == 0,
              (finalDirectory.st_mode & S_IFMT) == S_IFDIR, finalDirectory.st_uid == getuid(),
              (finalDirectory.st_mode & 0o777) == 0o700,
              finalDirectory.st_dev == dir.st_dev, finalDirectory.st_ino == dir.st_ino,
              ProcessStamp.read(pid: pid) == ProcessStamp(seconds: start_seconds, micros: start_micros) else { throw invalid }
        return OwnedDocumentReceiverRegistration(registrationID: registration_id, pid: pid,
            process: ProcessStamp(seconds: start_seconds, micros: start_micros),
            executablePath: executable_path, bundleID: bundle_id,
            documentCanonicalPath: document_path, documentSHA256: document_sha256,
            privateTemporaryDocument: true, plainTextDocument: true,
            targetWasNotPreexisting: true, processStartedWithinLaunchInterval: true)
    }
}

private func ownedBootstrapCanonicalPath(_ path: String) -> String? {
    guard let canonical = path.withCString({ realpath($0, nil) }) else { return nil }
    defer { free(canonical) }
    return String(cString: canonical)
}

private func ownedBootstrapExecutable(pid: UInt32) -> String? {
    var bytes = [CChar](repeating: 0, count: 4096)
    guard proc_pidpath(Int32(pid), &bytes, UInt32(bytes.count)) > 0 else { return nil }
    return ownedBootstrapCanonicalPath(String(decoding: bytes.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }, as: UTF8.self))
}

private final class OwnedDocumentCancellation: Sendable {
    private let flag = OSAllocatedUnfairLock(initialState: false)
    var cancelled: Bool { flag.withLock { $0 } }
    func cancel() { flag.withLock { $0 = true } }
}

private struct OwnedDocumentReadDiagnostic: Encodable {
    let kind = "owned_document_read"
    let sequence: UInt64
    let observer_available: Bool
    let observer_reason: String?
    let evidence_present: Bool
    let profile_eligible: Bool
    let scope_complete: Bool
    let private_boundary_present: Bool
}

@MainActor
func observeOwnedDocument(bootstrap: OwnedDocumentBootstrap, request: NativeRequest) async throws {
    guard ["observe", "watch"].contains(request.command), request.bundle_id == bootstrap.bundle_id, request.pid == bootstrap.pid,
          request.text_policy == nil else { throw NativeFailure("private owned document request does not match its registration") }
    let registration = try bootstrap.verifiedRegistration()
    let permissions = SourceMain.preflight()
    guard permissions.accessibility else { throw NativeFailure("Accessibility permission unavailable") }
    let app = AppIdentity(bundle_id: registration.bundleID, name: "TextEdit", pid: registration.pid)
    let session = AXSession(app: app, request: request, ownedDocumentRegistration: registration)
    let streamID = UUID().uuidString.lowercased()
    var sequence: UInt64 = 0
    var diagnosticCount = 0
    let cancellation = OwnedDocumentCancellation()
    let signals: [any DispatchSourceSignal] = [SIGTERM, SIGINT, SIGHUP].map { number in
        signal(number, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: number,
            queue: DispatchQueue(label: "cu.source.owned-stop-\(number)"))
        source.setEventHandler { @Sendable in cancellation.cancel() }
        source.resume()
        return source
    }
    defer { for source in signals { source.cancel() } }
    do {
        repeat {
            if cancellation.cancelled { break }
            // Saving to a new inode requires a fresh Main-issued registration.
            _ = try bootstrap.verifiedRegistration()
            sequence += 1
            let started = UInt64(Date().timeIntervalSince1970 * 1000)
            let scope = OwnedDocumentReadScope(snapshotID: "\(streamID)-tree-\(sequence)", sourceSequence: sequence,
                observedMS: started, validUntilMS: started + UInt64(request.timeout) + 1_000, clockDomain: "native_unix_ms")
            let timer = SourceMain.watchdog(timeoutMS: request.timeout)
            let result = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) },
                                            ownedDocumentScope: scope)
            timer.cancel()
            if cancellation.cancelled { break }
            let diagnostic = OwnedDocumentReadDiagnostic(sequence: sequence,
                observer_available: result.observer_available, observer_reason: result.observer_reason,
                evidence_present: result.owned_document_evidence != nil,
                profile_eligible: result.owned_document_evidence?.profile_eligible ?? false,
                scope_complete: result.owned_document_evidence?.scope_complete ?? false,
                private_boundary_present: result.owned_document_identity_boundary != nil)
            if diagnosticCount < 8, var data = try? JSONEncoder().encode(diagnostic), data.count <= 4096 {
                diagnosticCount += 1
                data.append(0x0a)
                try? FileHandle.standardError.write(contentsOf: data)
            }
            var structure = result.read.structure
            structure.owned_document_evidence = nil
            _ = try bootstrap.verifiedRegistration()
            let now = UInt64(Date().timeIntervalSince1970 * 1000)
            if let evidence = result.owned_document_evidence, evidence.profile_eligible,
               evidence.provenance.sourceSequence == sequence,
               evidence.provenance.snapshotID == scope.snapshotID,
               evidence.provenance.clockDomain == scope.clockDomain,
               evidence.provenance.identityBoundary == result.owned_document_identity_boundary,
               now >= result.finished_unix_ms && now < evidence.provenance.validUntilMS {
                structure.owned_document_evidence = evidence
            }
            try SourceMain.emit(StreamObservationResponse(stream_id: streamID, sequence: sequence,
                received_uptime_ns: uptimeNs(), received_unix_ms: now, permissions: SourceMain.preflight(),
                structure: structure, structure_started_uptime_ns: result.read.started,
                structure_finished_uptime_ns: result.read.finished,
                structure_started_unix_ms: result.started_unix_ms, structure_finished_unix_ms: result.finished_unix_ms,
                frame_ref: nil, tree_revision: result.tree_revision, observer_revision: result.observer_revision,
                observer_available: false, observer_reason: "Generic AX observer coverage was not observed in private owned mode.",
                ocr: Ocr(), errors: ["Private owned scope does not provide generic AX completeness, screenshots or OCR."]))
            if request.command != "watch" { break }
            try await Task.sleep(for: .milliseconds(request.interval))
        } while !cancellation.cancelled
        await session.close()
    } catch {
        await session.close()
        throw error
    }
}
