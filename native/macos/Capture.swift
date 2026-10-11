import AppKit
import ScreenCaptureKit
import Vision
import ImageIO
import UniformTypeIdentifiers
import Darwin

struct NativeFailure: Error, CustomStringConvertible, Sendable {
    let description: String
    init(_ description: String) { self.description = description }
}

/// Vision boxes use lower-left normalized coordinates. Public observations use
/// global display points measured from the main display's upper-left corner.
func globalOcrRect(_ normalized: CGRect, window: Rect) -> Rect? {
    let components = [normalized.minX, normalized.minY, normalized.width, normalized.height]
    guard components.allSatisfy(\.isFinite), normalized.minX >= 0, normalized.minY >= 0,
          normalized.maxX <= 1.000_001, normalized.maxY <= 1.000_001,
          normalized.width > 0, normalized.height > 0,
          window.valid, window.width > 0, window.height > 0 else { return nil }
    return Rect(CGRect(x: window.x + normalized.minX * window.width,
                       y: window.y + (1 - normalized.maxY) * window.height,
                       width: normalized.width * window.width,
                       height: normalized.height * window.height))
}

func appendRecognizedText(_ text: String, confidence: Float, frame: Rect, to result: inout Ocr) {
    if text.utf8.count > 512 {
        result.complete = false
        result.reason = "OCR text byte budget exceeded"
    }
    result.blocks.append(OcrBlock(text: boundedText(text), confidence: confidence, frame: frame))
}

func recognizeText(image: CGImage, window: Rect, maxBlocks: Int) -> Ocr {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.minimumTextHeight = 0.008
    // Bounded pixels enter Vision; process-level watchdog also bounds legacy
    // Vision calls, whose synchronous perform API has no timeout parameter.
    do {
        try VNImageRequestHandler(cgImage: image, orientation: .up, options: [:]).perform([request])
        let results = request.results ?? []
        var result = Ocr(complete: results.count <= maxBlocks,
                         reason: results.count <= maxBlocks ? nil : "OCR block budget exceeded")
        for observation in results.prefix(maxBlocks) {
            guard let candidate = observation.topCandidates(1).first,
                  candidate.confidence.isFinite,
                  let rect = globalOcrRect(observation.boundingBox, window: window) else {
                result.complete = false; result.reason = "OCR result geometry unavailable"; continue
            }
            appendRecognizedText(candidate.string, confidence: candidate.confidence, frame: rect, to: &result)
        }
        return result
    } catch {
        let failure = error as NSError
        return Ocr(complete: false, reason: "Vision OCR failed: \(failure.domain)/\(failure.code)", blocks: [])
    }
}

/// Generate fresh assets only in a caller-owned private directory. openat plus
/// O_EXCL/O_NOFOLLOW prevents replacement of existing files and symlink targets.
func savePNG(_ image: CGImage, outputDir: String) throws -> (String, Int) {
    guard outputDir.hasPrefix("/"), outputDir.utf8.count <= 4096 else { throw NativeFailure("asset directory must be absolute") }
    let directory = open(outputDir, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    guard directory >= 0 else { throw NativeFailure("cannot open private asset directory") }
    defer { close(directory) }
    var info = stat()
    guard fstat(directory, &info) == 0, info.st_uid == getuid(), info.st_mode & 0o777 == 0o700 else {
        throw NativeFailure("asset directory must be owned by this user with mode 0700")
    }
    let data = NSMutableData()
    guard let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else {
        throw NativeFailure("PNG encoder unavailable")
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination), data.length > 0, data.length <= 12 * 1024 * 1024 else {
        throw NativeFailure("PNG asset exceeds byte budget")
    }
    let name = "capture-\(UUID().uuidString.lowercased()).png"
    let file = openat(directory, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
    guard file >= 0 else { throw NativeFailure("cannot create fresh capture asset") }
    var success = false
    defer {
        close(file)
        if !success { unlinkat(directory, name, 0) }
    }
    var offset = 0
    while offset < data.length {
        let written = write(file, data.bytes.advanced(by: offset), data.length - offset)
        if written < 0, errno == EINTR { continue }
        guard written > 0 else { throw NativeFailure("capture asset write failed") }
        offset += written
    }
    success = true
    return (name, data.length)
}

/// AppKit target identity is main-actor isolated. SCScreenshotManager provides
/// one image, not SCStream frame status/display time/idle liveness. The request
/// interval is explicitly conservative evidence time, never a guessed frame PTS.
@MainActor
func captureWindow(app: AppIdentity, window: WindowInfo, request: NativeRequest, initialRead: StructureRead) async throws -> (Frame, Ocr) {
    guard captureReadPrivacyEligible(initialRead, window: window) else {
        throw NativeFailure("initial window privacy coverage unavailable")
    }
    guard CGPreflightScreenCaptureAccess(), AXIsProcessTrusted() else {
        throw NativeFailure("capture permissions unavailable")
    }
    guard let output = request.output_dir else { throw NativeFailure("private asset directory is required") }
    guard let id = UInt32(window.window_id.replacingOccurrences(of: "window-", with: "")) else {
        throw NativeFailure("target window identity unavailable")
    }
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    let matches = content.windows.filter { $0.windowID == id && $0.owningApplication?.processID == pid_t(app.pid) }
    guard matches.count == 1, let scWindow = matches.first,
          scWindow.owningApplication?.bundleIdentifier == app.bundle_id,
          Rect(scWindow.frame).approximatelyEquals(window.frame) else {
        throw NativeFailure("target window changed before capture")
    }
    let before = targetWindows(pid: app.pid).first { $0.id == id }
    guard let before, before.frame.approximatelyEquals(window.frame) else {
        throw NativeFailure("target window is no longer on screen")
    }
    let current = NSRunningApplication(processIdentifier: pid_t(app.pid))
    guard current?.bundleIdentifier == app.bundle_id, current?.isTerminated == false,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid) else {
        throw NativeFailure("target application identity changed")
    }
    let config = SCStreamConfiguration()
    let scale = min(1, min(2048 / window.frame.width, 1536 / window.frame.height))
    config.width = max(1, Int((window.frame.width * scale).rounded()))
    config.height = max(1, Int((window.frame.height * scale).rounded()))
    config.scalesToFit = true
    config.showsCursor = false
    config.ignoreShadowsSingleWindow = true
    if #available(macOS 14.2, *) { config.includeChildWindows = false }
    let filter = SCContentFilter(desktopIndependentWindow: scWindow)
    let started = uptimeNs()
    guard initialRead.finished <= started,
          started - initialRead.finished <= UInt64(request.timeout) * 1_000_000 else {
        throw NativeFailure("initial AX privacy coverage expired before capture")
    }
    let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
    let finished = uptimeNs()
    guard image.width > 0, image.height > 0, image.width <= 2048, image.height <= 1536,
          finished - started <= UInt64(request.timeout) * 1_000_000 else {
        throw NativeFailure("capture exceeded size or time budget")
    }
    guard CGPreflightScreenCaptureAccess(), AXIsProcessTrusted(),
          let after = targetWindows(pid: app.pid).first(where: { $0.id == id }),
          after.frame == before.frame, after.pid == before.pid,
          NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid) else {
        throw NativeFailure("target window or permission changed during capture")
    }
    let scaleX = Double(image.width) / window.frame.width
    let scaleY = Double(image.height) / window.frame.height
    guard abs(scaleX - scaleY) < 0.01 else { throw NativeFailure("capture pixel transform is nonuniform") }
    // Nothing is saved or passed to OCR before the second AX read establishes
    // that the known privacy context did not change across capture.
    let reread = await readStructure(app: app, frontmostPID: app.pid, request: request)
    guard captureReadPrivacyEligible(reread, window: window), reread.started >= finished else {
        let scopeState: String
        if let search = reread.structure.table_scope_search {
            switch search.state {
            case .complete: scopeState = "complete,candidates=\(search.candidates.count)"
            case .incomplete(let reason): scopeState = "incomplete:\(reason)"
            }
        } else { scopeState = "unavailable" }
        throw NativeFailure("AX privacy reread incomplete: \(reread.structure.incomplete_reason ?? "unknown"); scope_state=\(scopeState)")
    }
    guard reread.secureFrames.isEmpty else { throw NativeFailure("sensitive fields appeared across capture") }
    guard reread.structure.active_layer == initialRead.structure.active_layer else {
        throw NativeFailure("target active layer changed across capture")
    }
    guard unchangedPrivacyContext(before: initialRead, after: reread) else {
        throw NativeFailure("target content or geometry changed across capture")
    }
    guard NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid) else {
        throw NativeFailure("frontmost application changed across capture")
    }
    let ocr = recognizeText(image: image, window: window.frame, maxBlocks: request.ocrLimit)
    if initialRead.visiblePrivacy != nil {
        let publicationRead = await readStructure(app: app, frontmostPID: app.pid, request: request)
        guard captureReadPrivacyEligible(publicationRead, window: window),
              unchangedPrivacyContext(before: reread, after: publicationRead),
              CGPreflightScreenCaptureAccess(), AXIsProcessTrusted(),
              NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid),
              NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
              targetWindows(pid: app.pid).contains(where: { $0.id == id && $0.frame == before.frame }),
              uptimeNs() - started <= UInt64(request.timeout) * 1_000_000 else {
            throw NativeFailure("visible window privacy changed before capture publication")
        }
    }
    let (file, bytes) = try savePNG(image, outputDir: output)
    return (Frame(window_id: window.window_id, pid: app.pid, frame: window.frame,
                  width_px: image.width, height_px: image.height, scale_factor: scaleX,
                  started_uptime_ns: started, finished_uptime_ns: finished,
                  png_file: file, byte_length: bytes), ocr)
}
