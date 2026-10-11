import AppKit
import ApplicationServices
import Darwin

@main
struct SourceMain {
    static let maxInput = 8192
    static let maxOutput = 768 * 1024

    @MainActor
    static func emit<T: Encodable>(_ value: T) throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let dataValue: any Encodable
        if var observation = value as? StreamObservationResponse {
            observation.structure.global_focus = reconcileGlobalFocusObservation(
                read: observation.structure.global_focus, current: globalFocusObservation())
            observation.structure.normalizeSourceProvenance(sequence: observation.sequence,
                observedMS: observation.structure_finished_unix_ms)
            dataValue = observation
        } else if var observation = value as? ObservationResponse {
            observation.structure.global_focus = reconcileGlobalFocusObservation(
                read: observation.structure.global_focus, current: globalFocusObservation())
            dataValue = observation
        } else { dataValue = value }
        var data = try encoder.encode(dataValue)
        guard data.count < maxOutput else { throw NativeFailure("response exceeds JSON byte budget") }
        data.append(0x0a)
        try FileHandle.standardOutput.write(contentsOf: data)
    }

    @MainActor
    private static func globalFocusObservation() -> GlobalFocusObservation {
        guard let frontmost = NSWorkspace.shared.frontmostApplication else {
            return GlobalFocusObservation(app: .absent, window_id: .unavailable)
        }
        guard let bundle = frontmost.bundleIdentifier, !bundle.isEmpty,
              frontmost.processIdentifier > 0 else {
            return GlobalFocusObservation(app: .unavailable, window_id: .unavailable)
        }
        let localizedName = frontmost.localizedName ?? bundle
        let name = localizedName.isEmpty
            || localizedName.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
            ? bundle : boundedText(localizedName)
        return GlobalFocusObservation(
            app: .known(AppIdentity(bundle_id: bundle,
                name: name,
                pid: UInt32(frontmost.processIdentifier))),
            // The Source lane does not make a separate unbounded system-wide
            // focused-window query; target identity is never substituted here.
            window_id: .unavailable)
    }

    static func preflight() -> PermissionState {
        // Neither API requests permission or opens a System Settings prompt.
        PermissionState(accessibility: AXIsProcessTrusted(), screen_capture: CGPreflightScreenCaptureAccess())
    }

    /// This independent timer is the hard bound around uncooperative platform
    /// calls. The Rust parent also terminates overdue children. No partial
    /// observation is emitted on watchdog exit; callers mark it unavailable.
    static func watchdog(timeoutMS: Int) -> any DispatchSourceTimer {
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "cu.source.watchdog"))
        timer.schedule(deadline: .now() + .milliseconds(timeoutMS))
        timer.setEventHandler {
            let message = "native observation request exceeded its time budget\n"
            message.withCString { _ = write(STDERR_FILENO, $0, strlen($0)) }
            _exit(124)
        }
        timer.resume()
        return timer
    }

    private static func readByte(_ fileDescriptor: Int32) throws -> UInt8? {
        var byte: UInt8 = 0
        while true {
            let count = Darwin.read(fileDescriptor, &byte, 1)
            if count == 1 { return byte }
            if count == 0 { return nil }
            if errno == EINTR { continue }
            throw NativeFailure("cannot read bounded source request")
        }
    }

    private static func readStartupLine(fileDescriptor: Int32) throws -> (data: Data, terminated: Bool) {
        var data = Data()
        while true {
            guard let byte = try readByte(fileDescriptor) else { return (data, false) }
            if byte == 0x0a { return (data, true) }
            guard data.count < maxInput else { throw NativeFailure("request exceeds 8192 bytes") }
            data.append(byte)
        }
    }

    private static func appendUntilEOF(_ data: inout Data, fileDescriptor: Int32) throws {
        var buffer = [UInt8](repeating: 0, count: 4096)
        while true {
            let remaining = maxInput + 1 - data.count
            guard remaining > 0 else { throw NativeFailure("request exceeds 8192 bytes") }
            let count = Darwin.read(fileDescriptor, &buffer, min(buffer.count, remaining))
            if count > 0 {
                data.append(contentsOf: buffer.prefix(count))
                guard data.count <= maxInput else { throw NativeFailure("request exceeds 8192 bytes") }
                continue
            }
            if count == 0 { return }
            if errno == EINTR { continue }
            throw NativeFailure("cannot read bounded source request")
        }
    }

    private static func startupInput(fileDescriptor: Int32, ownedBootstrap: Bool) throws -> Data {
        // The private owned-document bootstrap keeps its original EOF-delimited
        // request framing. Ordinary one-shot commands also remain EOF-delimited
        // after the first line, which preserves trailing-data rejection.
        if ownedBootstrap {
            var data = Data()
            try appendUntilEOF(&data, fileDescriptor: fileDescriptor)
            return data
        }
        let (firstLine, terminated) = try readStartupLine(fileDescriptor: fileDescriptor)
        guard !firstLine.isEmpty else { throw NativeFailure("request is empty") }
        let isWatch = ((try? JSONSerialization.jsonObject(with: firstLine)) as? [String: Any])?["command"] as? String == "watch"
        if isWatch {
            guard terminated, firstLine.count + 1 <= maxInput else {
                throw NativeFailure("watch startup request must be one bounded newline-terminated record")
            }
            return firstLine
        }
        var data = firstLine
        if terminated { data.append(0x0a) }
        try appendUntilEOF(&data, fileDescriptor: fileDescriptor)
        return data
    }

    static func decodeRequest(_ data: Data) throws -> NativeRequest {
        guard !data.isEmpty, data.count <= maxInput else { throw NativeFailure("request is empty or exceeds 8192 bytes") }
        let allowed = Set(["protocol_version", "command", "bundle_id", "pid", "output_dir", "timeout_ms", "max_nodes", "max_depth", "max_ocr_blocks", "interval_ms", "clock_origin_unix_ms", "text_policy", "table_scope", "background_target", "session_epoch", "watch_id", "surface"])
        guard let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(fields.keys).isSubset(of: allowed) else { throw NativeFailure("request contains unknown fields") }
        let request = try JSONDecoder().decode(NativeRequest.self, from: data)
        if fields["background_target"] != nil {
            guard let raw = fields["background_target"] as? [String: Any],
                  (Set(raw.keys) == ["window_id"] || Set(raw.keys) == ["window_id", "process"]),
                  raw["window_id"] is String,
                  request.validBackgroundTarget(), request.background_target != nil else {
                throw NativeFailure("background AX target scope invalid")
            }
            if let process = raw["process"] {
                guard let rawProcess = process as? [String: Any],
                      Set(rawProcess.keys) == ["pid", "start_seconds", "start_micros"],
                      safeJSONInteger(rawProcess["pid"]), safeJSONInteger(rawProcess["start_seconds"]),
                      safeJSONInteger(rawProcess["start_micros"]) else {
                    throw NativeFailure("background AX process birth scope invalid")
                }
            }
        } else if request.background_target != nil || !request.validBackgroundTarget() {
            throw NativeFailure("background AX target scope invalid")
        }
        if let policy = request.text_policy {
            guard let raw = fields["text_policy"] as? [String: Any], Set(raw.keys) == ["kind", "window_id"],
                  policy.valid(bundle: request.bundle_id, pid: request.pid) else { throw NativeFailure("registered text policy scope invalid") }
        }
        if request.table_scope != nil {
            guard ["observe", "watch"].contains(request.command),
                  let policy = request.text_policy, policy.valid(bundle: request.bundle_id, pid: request.pid),
                  let raw = fields["table_scope"] as? [String: Any],
                  try JSONSerialization.data(withJSONObject: raw).count <= 4096 else {
                throw NativeFailure("table scope requires a bounded fixed registered process/window request")
            }
        }
        return request
    }

    @MainActor
    static func main() async {
        do {
            let arguments = Array(CommandLine.arguments.dropFirst())
            guard arguments.isEmpty || arguments == ["--owned-document-bootstrap"] else {
                throw NativeFailure("unsupported private source arguments")
            }
            let ownedBootstrap = arguments.isEmpty ? nil : try OwnedDocumentBootstrap.read(from: FileHandle.standardInput)
            let data = try startupInput(fileDescriptor: STDIN_FILENO, ownedBootstrap: ownedBootstrap != nil)
            guard !data.isEmpty, data.count <= maxInput else { throw NativeFailure("request is empty or exceeds 8192 bytes") }
            let request = try decodeRequest(data)
            let fields = try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
            guard request.protocol_version == 1 else { throw NativeFailure("unsupported protocol version") }
            if ownedBootstrap == nil, request.command == "watch" {
                guard let scope = request.watchScope, safeNativeWatchScope(scope),
                      let rawSurface = fields["surface"] as? [String: Any],
                      Set(rawSurface.keys) == ["id", "epoch"],
                      safeJSONInteger(fields["session_epoch"]),
                      safeJSONInteger(fields["clock_origin_unix_ms"]),
                      safeJSONInteger(rawSurface["epoch"]) else {
                    throw NativeFailure("watch startup identity or clock scope invalid")
                }
            } else if request.command != "watch", fields["watch_id"] != nil || fields["surface"] != nil {
                throw NativeFailure("watch scope fields are valid only for the watch command")
            }
            if request.background_target != nil {
                guard ownedBootstrap == nil, request.command == "watch", request.table_scope == nil,
                      request.text_policy == nil else {
                    throw NativeFailure("background AX target cannot combine with another Source scope")
                }
            }
            if let ownedBootstrap {
                guard fields["watch_id"] == nil, fields["surface"] == nil else {
                    throw NativeFailure("private owned-document request cannot carry watch-control identity")
                }
                try await observeOwnedDocument(bootstrap: ownedBootstrap, request: request)
                return
            }
            let timer = request.command == "watch" ? nil : watchdog(timeoutMS: request.command == "self_test" ? 10_000 : request.timeout)
            defer { timer?.cancel() }
            let permissions = preflight()
            switch request.command {
            case "permissions":
                try emit(PermissionsResponse(accessibility: permissions.accessibility, screen_capture: permissions.screen_capture))
            case "self_test":
                let response = try runSelfTests()
                let readerTests = try await runWatchControlReaderTests()
                try emit(SelfTestResponse(test_count: response.test_count + readerTests.count,
                                          tests: response.tests + readerTests))
            case "watch":
                guard let bundle = request.bundle_id, validBundleID(bundle) else { throw NativeFailure("valid target bundle_id is required") }
                try await watchSource(request: request)
            case "observe":
                guard let bundle = request.bundle_id, validBundleID(bundle) else {
                    throw NativeFailure("valid target bundle_id is required")
                }
                let apps = NSRunningApplication.runningApplications(withBundleIdentifier: bundle)
                    .filter { !$0.isTerminated && (request.pid == nil || UInt32($0.processIdentifier) == request.pid) }
                let selected = apps.count == 1 ? apps.first : nil
                let identity = AppIdentity(bundle_id: bundle, name: boundedText(selected?.localizedName ?? bundle),
                                           pid: selected.map { UInt32($0.processIdentifier) } ?? request.pid ?? 0)
                var response = ObservationResponse(permissions: permissions, structure: Structure(app: identity),
                    structure_started_uptime_ns: uptimeNs(), structure_finished_uptime_ns: uptimeNs())
                guard let selected, selected.bundleIdentifier == bundle else {
                    response.structure.incomplete_reason = "target app missing or ambiguous"
                    response.errors = ["target app missing or ambiguous"]
                    try emit(response); return
                }
                guard permissions.accessibility else {
                    response.structure.incomplete_reason = "Accessibility permission unavailable"
                    response.errors = ["Accessibility permission unavailable"]
                    try emit(response); return
                }
                let frontmost = NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) }
                let read = await readStructure(app: identity, frontmostPID: frontmost, request: request)
                response.structure = read.structure
                response.structure_started_uptime_ns = read.started
                response.structure_finished_uptime_ns = read.finished
                let foregroundStable = NSWorkspace.shared.frontmostApplication.map({ UInt32($0.processIdentifier) }) == frontmost
                if !foregroundStable {
                    response.structure.active_layer = nil
                    response.structure.complete = false
                    response.structure.incomplete_reason = "frontmost application changed during AX read"
                }
                if !permissions.screen_capture { response.errors.append("Screen Recording permission unavailable") }
                else if !foregroundStable { response.errors.append("capture withheld because frontmost application changed") }
                else if !read.secureFrames.isEmpty { response.errors.append("capture withheld because target contains sensitive text fields") }
                else if let active = read.structure.active_layer,
                        let window = read.structure.windows.first(where: { $0.window_id == active }) {
                    if !captureReadPrivacyEligible(read, window: window) {
                        response.errors.append("capture withheld because visible window privacy coverage is incomplete")
                    } else {
                        do {
                            let (frame, ocr) = try await captureWindow(app: identity, window: window, request: request, initialRead: read)
                            response.frame = frame; response.ocr = ocr
                        } catch { response.errors.append(boundedText(String(describing: error))) }
                    }
                } else { response.errors.append("capture withheld because target active layer is unresolved") }
                try emit(response)
            default: throw NativeFailure("unknown command")
            }
        } catch {
            if shouldEmitGenericErrorResponse(for: error) {
                try? emit(ErrorResponse(error: boundedText(String(describing: error))))
            }
            exit(2)
        }
    }
}
