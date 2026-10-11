import AppKit
import ScreenCaptureKit
import CoreMedia
import CoreVideo
import Darwin
import os

struct StreamSampleMetadata: Sendable {
    let status: String
    let displayTicks: UInt64?
    let displayNs: UInt64?
    let receivedNs: UInt64
    let receivedUnixMs: UInt64
    let width: Int
    let height: Int
    let scaleFactor: Double
    let contentScale: Double
    let contentRect: Rect
    let dirtyRects: [Rect]
}

private struct StreamGeometry: Equatable {
    let width: Int
    let height: Int
    let scale: Double
    let contentScale: Double
    let content: Rect
    init(_ metadata: StreamSampleMetadata) {
        width = metadata.width; height = metadata.height
        scale = metadata.scaleFactor; contentScale = metadata.contentScale; content = metadata.contentRect
    }
}

struct OwnedStreamSample: Sendable {
    let metadata: StreamSampleMetadata
    let pixels: Data
    let bytesPerRow: Int

    func image() -> CGImage? {
        guard pixels.count <= 16 * 1024 * 1024,
              let provider = CGDataProvider(data: pixels as CFData) else { return nil }
        return CGImage(width: metadata.width, height: metadata.height, bitsPerComponent: 8,
                       bitsPerPixel: 32, bytesPerRow: bytesPerRow,
                       space: CGColorSpaceCreateDeviceRGB(),
                       bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.premultipliedFirst.rawValue)
                           .union(.byteOrder32Little),
                       provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }
}

/// A single owned pixel copy plus one latest control status. The capture queue
/// never waits for AX, OCR, filesystem or stdout. Its short lock protects only
/// typed Sendable values; pixel buffers and CMSampleBuffers never leave callback.
final class StreamMailbox: Sendable {
    private struct RefreshFilter: Sendable {
        let boundary: UInt64
        let width: Int
        let height: Int
    }
    private struct State {
        var pixels: OwnedStreamSample?
        var status: StreamSampleMetadata?
        var lastMetadata: StreamSampleMetadata?
        var lastCallbackNs: UInt64 = 0
        var dropped: UInt64 = 0
        var failure: String?
        var cancelled = false
        var refresh: RefreshFilter?
    }
    private let state = OSAllocatedUnfairLock(initialState: State())

    func fallback() -> StreamSampleMetadata? { state.withLock { $0.lastMetadata } }
    func offer(_ metadata: StreamSampleMetadata, pixels: OwnedStreamSample?) {
        state.withLock {
            let previousCallback = $0.lastCallbackNs
            $0.lastCallbackNs = metadata.receivedNs
            $0.lastMetadata = metadata
            // A known negative remains latched through endRefresh until the
            // reader drains it; a later idle/full callback cannot erase it.
            if ["blank", "suspended", "stopped"].contains($0.status?.status ?? "") { return }
            if let refresh = $0.refresh {
                if metadata.receivedNs < previousCallback || pixels != nil &&
                    (metadata.displayTicks == nil || metadata.displayTicks == 0 || metadata.displayNs == nil || metadata.displayNs == 0
                     || metadata.displayNs.map { $0 > metadata.receivedNs } == true) {
                    $0.failure = "SCStream invalid complete clock or callback chronology during refresh"
                    $0.pixels = nil
                    return
                }
                if pixels != nil && streamRefreshTransitionDiscardable(metadata, boundary: refresh.boundary,
                    width: refresh.width, height: refresh.height) { return }
                if pixels != nil && (metadata.width != refresh.width || metadata.height != refresh.height) {
                    $0.failure = "SCStream unexpected fresh pixel geometry during refresh"
                    $0.pixels = nil
                    return
                }
                if pixels == nil && ["idle", "started"].contains(metadata.status) { return }
            }
            if let pixels {
                if $0.pixels != nil { $0.dropped += 1 }
                $0.pixels = pixels
            } else {
                $0.status = metadata
                if ["blank", "suspended", "stopped"].contains(metadata.status) { $0.pixels = nil }
            }
        }
    }
    func beginRefresh(boundary: UInt64, width: Int, height: Int) {
        state.withLock {
            $0.pixels = nil; $0.dropped = 0
            if !["blank", "suspended", "stopped"].contains($0.status?.status ?? "") { $0.status = nil }
            $0.refresh = RefreshFilter(boundary: boundary, width: width, height: height)
        }
    }
    func endRefresh() { state.withLock { $0.refresh = nil } }
    func fail(_ reason: String) { state.withLock { $0.failure = boundedText(reason); $0.pixels = nil } }
    func cancel() { state.withLock { $0.cancelled = true } }
    var cancelled: Bool { state.withLock { $0.cancelled } }
    func captureAllowed(nowNS: UInt64, timeoutNS: UInt64) -> Bool {
        state.withLock {
            !$0.cancelled && $0.failure == nil && !["blank", "suspended", "stopped"].contains($0.status?.status ?? "")
                && $0.lastCallbackNs > 0 && nowNS >= $0.lastCallbackNs && nowNS - $0.lastCallbackNs < timeoutNS
        }
    }
    func drain() -> (OwnedStreamSample?, StreamSampleMetadata?, UInt64, UInt64, String?) {
        state.withLock {
            let result = ($0.pixels, $0.status, $0.dropped, $0.lastCallbackNs, $0.failure)
            $0.pixels = nil; $0.status = nil; $0.dropped = 0
            return result
        }
    }
}

func machTicksToNs(_ ticks: UInt64) -> UInt64? {
    var info = mach_timebase_info_data_t()
    guard mach_timebase_info(&info) == KERN_SUCCESS, info.denom != 0 else { return nil }
    let divisor = UInt64(info.denom); let multiplier = UInt64(info.numer)
    let whole = (ticks / divisor).multipliedReportingOverflow(by: multiplier)
    let fraction = (ticks % divisor).multipliedReportingOverflow(by: multiplier)
    guard !whole.overflow, !fraction.overflow else { return nil }
    let result = whole.partialValue.addingReportingOverflow(fraction.partialValue / divisor)
    return result.overflow ? nil : result.partialValue
}

func streamSourceUnixMs(_ metadata: StreamSampleMetadata) -> UInt64? {
    guard let display = metadata.displayNs, display <= metadata.receivedNs else { return nil }
    let difference = metadata.receivedNs - display
    let age = difference / 1_000_000 + (difference % 1_000_000 == 0 ? 0 : 1)
    guard metadata.receivedUnixMs >= age else { return nil }
    return metadata.receivedUnixMs - age
}

private func attachedRect(_ value: Any?) -> Rect? {
    let rect: CGRect?
    if let value = value as? CGRect { rect = value }
    else if let value = value as? NSValue { rect = value.rectValue }
    else if let dictionary = value as? [String: Any] { rect = CGRect(dictionaryRepresentation: dictionary as CFDictionary) }
    else { rect = nil }
    guard let rect else { return nil }
    let result = Rect(rect)
    return result.valid ? result : nil
}

private func frameStatus(_ value: Int) -> String? {
    switch SCFrameStatus(rawValue: value) {
    case .complete: return "complete"
    case .idle: return "idle"
    case .blank: return "blank"
    case .suspended: return "suspended"
    case .started: return "started"
    case .stopped: return "stopped"
    default: return nil
    }
}

/// SCStreamFrameInfo.contentRect is in surface points, while the CVPixelBuffer
/// dimensions are pixels. Convert with the display's scaleFactor first. The
/// full-window OCR transform is valid only when content fills the buffer.
func streamContentCoversPixels(_ metadata: StreamSampleMetadata) -> Bool {
    guard metadata.contentRect.valid, metadata.scaleFactor.isFinite,
          (1...4).contains(metadata.scaleFactor), metadata.width > 0, metadata.height > 0 else { return false }
    let content = metadata.contentRect
    let pixels = Rect(CGRect(x: content.x * metadata.scaleFactor, y: content.y * metadata.scaleFactor,
                             width: content.width * metadata.scaleFactor, height: content.height * metadata.scaleFactor))
    return pixels.approximatelyEquals(Rect(CGRect(x: 0, y: 0, width: metadata.width, height: metadata.height)))
}

final class StreamSink: NSObject, SCStreamOutput, SCStreamDelegate, Sendable {
    let mailbox: StreamMailbox
    init(mailbox: StreamMailbox) { self.mailbox = mailbox }

    func stream(_ stream: SCStream, didStopWithError error: any Error) {
        let error = error as NSError
        mailbox.fail("SCStream stopped: \(error.domain)/\(error.code)")
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of outputType: SCStreamOutputType) {
        guard outputType == .screen, CMSampleBufferIsValid(sampleBuffer), !mailbox.cancelled else { return }
        let receivedNs = uptimeNs(); let receivedUnixMs = unixMs()
        guard let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false)
            as? [[SCStreamFrameInfo: Any]], let info = attachments.first,
              let statusCode = info[.status] as? NSNumber,
              let status = frameStatus(statusCode.intValue) else {
            mailbox.fail("SCStream frame status unavailable"); return
        }
        let previous = mailbox.fallback()
        let buffer = CMSampleBufferGetImageBuffer(sampleBuffer)
        let width = buffer.map { CVPixelBufferGetWidth($0) } ?? previous?.width ?? 0
        let height = buffer.map { CVPixelBufferGetHeight($0) } ?? previous?.height ?? 0
        let ticks = (info[.displayTime] as? NSNumber)?.uint64Value
        let scale = (info[.scaleFactor] as? NSNumber)?.doubleValue ?? previous?.scaleFactor ?? 0
        let contentScale = (info[.contentScale] as? NSNumber)?.doubleValue ?? previous?.contentScale ?? 0
        let contentRect = attachedRect(info[.contentRect]) ?? previous?.contentRect
        let rawDirty = info[.dirtyRects] as? [Any] ?? []
        guard rawDirty.count <= 256, let contentRect,
              width > 0, height > 0, width <= 2048, height <= 1536,
              scale.isFinite, scale > 0, contentScale.isFinite, contentScale > 0,
              contentRect.width > 0, contentRect.height > 0 else {
            mailbox.fail("SCStream frame geometry exceeds bounds or is unavailable"); return
        }
        let dirty = rawDirty.compactMap { attachedRect($0) }
        guard dirty.count == rawDirty.count else { mailbox.fail("SCStream dirty rectangle invalid"); return }
        let metadata = StreamSampleMetadata(status: status, displayTicks: ticks,
            displayNs: ticks.flatMap(machTicksToNs), receivedNs: receivedNs, receivedUnixMs: receivedUnixMs,
            width: width, height: height, scaleFactor: scale, contentScale: contentScale,
            contentRect: contentRect, dirtyRects: dirty)
        guard status == "complete" else { mailbox.offer(metadata, pixels: nil); return }
        guard let buffer, ticks != nil, metadata.displayNs != nil,
              CVPixelBufferGetPixelFormatType(buffer) == kCVPixelFormatType_32BGRA,
              !CVPixelBufferIsPlanar(buffer),
              CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess else {
            mailbox.fail("SCStream complete pixels or source timestamp unavailable"); return
        }
        defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        let row = CVPixelBufferGetBytesPerRow(buffer)
        guard row >= width * 4, row <= width * 4 + 1024, row * height <= 16 * 1024 * 1024,
              let address = CVPixelBufferGetBaseAddress(buffer) else {
            mailbox.fail("SCStream pixel copy exceeds bounds"); return
        }
        let owned = OwnedStreamSample(metadata: metadata, pixels: Data(bytes: address, count: row * height), bytesPerRow: row)
        mailbox.offer(metadata, pixels: owned)
    }
}

@MainActor
private final class StreamWriter {
    let scope: NativeWatchScope
    let id: String
    init(scope: NativeWatchScope) {
        self.scope = scope
        self.id = scope.watchID
    }
    var sequence: UInt64 = 0
    var health: [String: String] = [:]
    func next() -> UInt64 { sequence += 1; return sequence }
    func health(source: String, state: String, reason: String?, ns: UInt64 = uptimeNs(), wall: UInt64 = unixMs()) throws {
        let signature = state + (reason ?? "")
        guard health[source] != signature else { return }
        health[source] = signature
        try SourceMain.emit(StreamHealthResponse(stream_id: id, sequence: next(), received_uptime_ns: ns,
            received_unix_ms: wall, source: source, state: state, reason: reason))
    }
    func gap(_ reason: String, dropped: UInt64 = 0, ns: UInt64 = uptimeNs(), wall: UInt64 = unixMs()) throws {
        try SourceMain.emit(StreamGapResponse(stream_id: id, sequence: next(), received_uptime_ns: ns,
            received_unix_ms: wall, dropped_frames: dropped, reason: reason))
    }
    func terminalUnavailable(_ reason: String) throws {
        let sequence = next()
        guard let record = streamTerminalResponse(scope: scope, sequence: sequence,
                receivedUptimeNS: uptimeNs(), receivedUnixMS: unixMs(), reason: reason) else {
            throw NativeFailure("watch terminal health record failed validation")
        }
        try SourceMain.emit(record)
    }
}

func streamTerminalResponse(scope: NativeWatchScope, sequence: UInt64,
                            receivedUptimeNS: UInt64, receivedUnixMS: UInt64,
                            reason: String) -> StreamTerminalResponse? {
    let reason = boundedText(reason)
    guard safeNativeWatchScope(scope), sequence > 0, receivedUptimeNS > 0, receivedUnixMS > 0,
          !reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
    return StreamTerminalResponse(stream_id: scope.watchID,
        sequence: sequence, received_uptime_ns: receivedUptimeNS, received_unix_ms: receivedUnixMS,
        source: "screenshot", state: "unavailable", reason: reason,
        session_epoch: scope.sessionEpoch, surface: scope.surface)
}

@MainActor
private func prepareStream(app: AppIdentity, window: WindowInfo, request: NativeRequest, sink: StreamSink) async throws -> (SCStream, SCStreamConfiguration, SCContentFilter, SCWindow) {
    guard CGPreflightScreenCaptureAccess(), AXIsProcessTrusted(),
          let id = UInt32(window.window_id.replacingOccurrences(of: "window-", with: "")) else {
        throw NativeFailure("SCStream permission or target window unavailable")
    }
    let content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true)
    let matches = content.windows.filter { $0.windowID == id && $0.owningApplication?.processID == pid_t(app.pid) }
    guard matches.count == 1, let target = matches.first,
          target.owningApplication?.bundleIdentifier == app.bundle_id,
          Rect(target.frame).approximatelyEquals(window.frame) else { throw NativeFailure("SCStream target identity changed") }
    let config = SCStreamConfiguration()
    let sizeScale = min(1, min(2048 / window.frame.width, 1536 / window.frame.height))
    config.width = max(1, Int((window.frame.width * sizeScale).rounded()))
    config.height = max(1, Int((window.frame.height * sizeScale).rounded()))
    config.minimumFrameInterval = CMTime(value: 1, timescale: 2)
    config.queueDepth = 3
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.showsCursor = false
    config.scalesToFit = true
    config.ignoreShadowsSingleWindow = true
    config.capturesAudio = false
    if #available(macOS 14.2, *) { config.includeChildWindows = false }
    else { throw NativeFailure("watch requires macOS 14.2 to explicitly exclude child windows") }
    let filter = SCContentFilter(desktopIndependentWindow: target)
    if #available(macOS 15.2, *) {
        guard filter.includedWindows.count == 1, filter.includedWindows.first?.windowID == target.windowID else {
            throw NativeFailure("SCStream content filter does not isolate the target window")
        }
    }
    let stream = SCStream(filter: filter, configuration: config, delegate: sink)
    try stream.addStreamOutput(sink, type: .screen, sampleHandlerQueue: DispatchQueue(label: "cu.source.scstream", qos: .userInitiated))
    return (stream, config, filter, target)
}

/// DispatchSource's legacy callback API does not spell out actor isolation.
/// Create handlers outside MainActor so Swift runtime checks do not infer that
/// a signal delivered on our independent queue must execute on the main queue.
private func stopSignals(mailbox: StreamMailbox) -> [any DispatchSourceSignal] {
    signal(SIGTERM, SIG_IGN); signal(SIGINT, SIG_IGN); signal(SIGPIPE, SIG_IGN)
    let termination = DispatchSource.makeSignalSource(signal: SIGTERM, queue: DispatchQueue(label: "cu.source.termination"))
    let interruption = DispatchSource.makeSignalSource(signal: SIGINT, queue: DispatchQueue(label: "cu.source.interruption"))
    let stop: @Sendable () -> Void = { mailbox.cancel() }
    termination.setEventHandler(handler: stop); termination.resume()
    interruption.setEventHandler(handler: stop); interruption.resume()
    return [termination, interruption]
}

private func streamFrame(metadata: StreamSampleMetadata, window: WindowInfo, app: AppIdentity,
                         revision: UInt64, id: String?, bytes: Int = 0, file: String? = nil,
                         pixelMetadata: StreamSampleMetadata? = nil) -> StreamFrame {
    let pixels = pixelMetadata ?? metadata
    return StreamFrame(status: metadata.status, display_time_ticks: pixels.displayTicks,
        display_time_uptime_ns: pixels.displayNs, source_unix_ms: streamSourceUnixMs(pixels),
        geometry_revision: revision, width_px: pixels.width, height_px: pixels.height,
        scale_factor: pixels.scaleFactor, content_scale: pixels.contentScale, content_rect: pixels.contentRect,
        dirty_rects: metadata.dirtyRects, frame_id: id, window_id: window.window_id, pid: app.pid,
        frame: window.frame, byte_length: bytes, png_file: file)
}

/// Fixed labels describe the retained read without exposing window identities
/// or target text. The status function is also the capture acceptance gate.
private func captureRevalidationPrivacySummary(before: StructureRead, after: StructureRead,
                                               window: WindowInfo) -> String {
    func layerState(_ read: StructureRead) -> String {
        guard let layer = read.structure.active_layer else { return "absent" }
        return layer == window.window_id ? "captured" : "other"
    }
    return "privacy_before=\(captureReadPrivacyStatus(before, window: window).rawValue),"
        + "privacy_after=\(captureReadPrivacyStatus(after, window: window).rawValue),"
        + "reader_before=\(before.privacyRejection?.rawValue ?? "none"),"
        + "reader_after=\(after.privacyRejection?.rawValue ?? "none"),"
        + "layer_before=\(layerState(before)),layer_after=\(layerState(after))"
}

/// Describe failed checks without logging field contents from a target app.
private func revalidationReason(before: StructureRead, after: StructureRead, metadata: StreamSampleMetadata,
                                window: WindowInfo, request: NativeRequest) -> String {
    var reasons: [String] = []
    if !captureReadPrivacyEligible(before, window: window) || !captureReadPrivacyEligible(after, window: window) { reasons.append("incomplete window privacy coverage") }
    if !before.secureFrames.isEmpty || !after.secureFrames.isEmpty { reasons.append("sensitive field") }
    if before.structure.active_layer != after.structure.active_layer {
        reasons.append("active layer changed")
    }
    if before.finished > metadata.receivedNs { reasons.append("AX baseline finished after frame callback") }
    else if metadata.receivedNs - before.finished > UInt64(request.timeout) * 1_000_000 { reasons.append("AX baseline expired") }
    if before.structure.objects.count != after.structure.objects.count {
        reasons.append("node count \(before.structure.objects.count) -> \(after.structure.objects.count)")
    }
    for (index, pair) in zip(before.structure.objects, after.structure.objects).enumerated() {
        let (a, b) = pair
        var fields: [String] = []
        if a.role != b.role || a.subrole != b.subrole { fields.append("role") }
        if a.window_id != b.window_id || a.frame != b.frame { fields.append("geometry") }
        if a.title != b.title || a.description != b.description || a.value != b.value { fields.append("content") }
        if a.secure != b.secure { fields.append("sensitivity") }
        if !fields.isEmpty { reasons.append("node[\(index)] \(fields.joined(separator: "/"))") }
        if reasons.count >= 6 { break }
    }
    if reasons.isEmpty { reasons.append("window geometry or foreground app changed") }
    return boundedText("AX revalidation: "
        + captureRevalidationPrivacySummary(before: before, after: after, window: window)
        + "; " + reasons.joined(separator: "; "))
}

/// SCStream filters one desktop-independent window and excludes child windows.
/// macOS adds a separate WindowSharingSessionButton window while recording;
/// changes to that uncaptured surface must not be compared as captured content.
/// Whole-tree or dedicated visible privacy coverage remains required; only
/// the content/geometry comparison is scoped to the captured ID.
private func capturedWindowRead(_ reading: StructureRead, windowID: String) -> StructureRead {
    var structure = reading.structure
    // A partial read's private coverage also audits its proved auxiliary root.
    // Preserve the original root inventory so eligibility can reject an extra
    // or mismatched window and compare the auxiliary content across readings.
    if structure.complete {
        structure.objects = structure.objects.filter { $0.window_id == windowID }
        structure.windows = structure.windows.filter { $0.window_id == windowID }
    }
    // Input activity is not a read permission. This private comparison omits
    // that execution fact; the published Structure keeps its actual layer/None.
    if structure.complete { structure.active_layer = nil }
    structure.window_context = nil
    return StructureRead(structure: structure, started: reading.started, finished: reading.finished,
                         secureFrames: reading.secureFrames, visiblePrivacy: reading.visiblePrivacy,
                         privacyRejection: reading.privacyRejection)
}

private func observationWindow(_ structure: Structure) -> WindowInfo? {
    if let active = structure.active_layer {
        return structure.windows.first { $0.window_id == active && !$0.modal }
    }
    let standard = structure.windows.filter { window in
        !window.modal && window.kind == "window" && structure.objects.contains {
            $0.window_id == window.window_id && $0.parent == nil && $0.role == "AXWindow" && $0.subrole == "AXStandardWindow"
        }
    }
    return standard.count == 1 ? standard.first : nil
}

private func streamReadPrivacyEligible(_ reading: StructureRead) -> Bool {
    guard let window = observationWindow(reading.structure) else { return false }
    return captureReadPrivacyEligible(reading, window: window)
}

func streamHasActiveModalCaptureMismatch(_ structure: Structure, capturedWindowID: String) -> Bool {
    guard let activeLayer = structure.active_layer, activeLayer != capturedWindowID else { return false }
    return structure.windows.contains { $0.window_id == activeLayer && $0.modal }
}

/// Bounded metadata for the already authorized target helps distinguish an
/// unreadable AX tree from a window that macOS removed from the current space.
/// This diagnostic never supplies capture authority or reads window titles.
@MainActor
private func targetWindowDiagnostics(pid: UInt32) -> String {
    let application = NSRunningApplication(processIdentifier: pid_t(pid))
    let hidden = application.map { String($0.isHidden) } ?? "unknown"
    let active = application.map { String($0.isActive) } ?? "unknown"
    guard let list = CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] else {
        return "target app hidden=\(hidden),active=\(active); CG metadata unavailable"
    }
    let windows = list.filter { ($0[kCGWindowOwnerPID as String] as? NSNumber)?.uint32Value == pid }
    let details = windows.prefix(3).map { entry in
        let id = (entry[kCGWindowNumber as String] as? NSNumber).map { String($0.uint32Value) } ?? "unknown"
        let screen = (entry[kCGWindowIsOnscreen as String] as? NSNumber).map { String($0.boolValue) } ?? "unknown"
        let layer = (entry[kCGWindowLayer as String] as? NSNumber).map { String($0.intValue) } ?? "unknown"
        var frame = "unknown"
        if let bounds = entry[kCGWindowBounds as String] as? [String: Any],
           let cgFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary), Rect(cgFrame).valid {
            frame = exactFrameDescription(Rect(cgFrame))
        }
        return "id=\(id),on_screen=\(screen),layer=\(layer),bounds=\(frame)"
    }
    return boundedText("target app hidden=\(hidden),active=\(active); CG windows=\(windows.count) [" + details.joined(separator: ";") + "]")
}

@MainActor
private func initialWatchFailure(_ reading: StructureRead, permissions: PermissionState) -> String {
    if !permissions.accessibility { return "initial Accessibility permission unavailable" }
    if !permissions.screen_capture { return "initial Screen Recording permission unavailable" }
    if !reading.structure.complete {
        return boundedText("initial AX snapshot incomplete: " + (reading.structure.incomplete_reason ?? "unknown reason")
            + "; " + targetWindowDiagnostics(pid: reading.structure.app.pid))
    }
    if !reading.secureFrames.isEmpty { return "initial capture privacy unresolved: sensitive fields present" }
    return "initial target window unavailable or ambiguous (AX windows=\(reading.structure.windows.count))"
}

private func precedingAXRead(_ readings: [AXSessionRead], displayNs: UInt64) -> AXSessionRead? {
    readings.last { $0.read.finished <= displayNs }
}

func streamRebaselineAllowed(_ current: StructureRead, window: WindowInfo,
                             nativeWindows: [NativeWindow], refreshAttempts: Int) -> Bool {
    refreshAttempts < 2 && captureReadPrivacyEligible(current, window: window)
        && current.structure.windows.contains { $0.window_id == window.window_id && $0.frame == window.frame && !$0.modal }
        && current.structure.objects.contains {
            $0.window_id == window.window_id && $0.parent == nil && $0.role == "AXWindow"
                && $0.subrole == "AXStandardWindow" && $0.frame == window.frame
        }
        && nativeWindows.contains {
            $0.ref == window.window_id && $0.pid == current.structure.app.pid
                && $0.frame == window.frame && $0.zIndex == window.z_index
        }
}

/// Rebind the stream's private baseline once, after SCStream has started but
/// before any pixels have been published. The new read must still describe the
/// same registered AX session, kernel process and selected CG window. Its own
/// privacy audit may include a newly appeared, fully audited auxiliary root;
/// no earlier pixel exists whose privacy context needs to be compared.
func streamStartupBaselineWindow(previous: AXSessionRead, previousWindow: WindowInfo,
                                 fresh: AXSessionRead, expectedSessionEpoch: UInt64,
                                 streamStartedNs: UInt64, currentProcess: ProcessStamp?,
                                 nativeWindows: [NativeWindow]) -> WindowInfo? {
    let previousWindows = previous.read.structure.windows.filter { $0.window_id == previousWindow.window_id }
    let freshWindows = fresh.read.structure.windows.filter { $0.window_id == previousWindow.window_id }
    guard streamStartedNs > 0,
          previousWindow.window_id.hasPrefix("window-"),
          previous.read.finished > 0, previous.read.finished <= streamStartedNs,
          fresh.read.started > streamStartedNs, fresh.read.finished > fresh.read.started,
          previous.captureRootContinuous == true, fresh.captureRootContinuous == true,
          let previousBoundary = previous.identity_boundary,
          let freshBoundary = fresh.identity_boundary,
          !previousBoundary.namespace.isEmpty,
          previousBoundary.namespace == freshBoundary.namespace,
          previousBoundary.generation > 0,
          freshBoundary.generation >= previousBoundary.generation,
          let previousContext = previous.read.structure.window_context,
          let freshContext = fresh.read.structure.window_context,
          !previousContext.source_session_id.isEmpty,
          previousContext.source_session_id == freshContext.source_session_id,
          previousContext.session_epoch == expectedSessionEpoch,
          freshContext.session_epoch == expectedSessionEpoch,
          previousContext.process == freshContext.process,
          previousContext.process.pid == previous.read.structure.app.pid,
          freshContext.process.pid == fresh.read.structure.app.pid,
          previousContext.origin_window_id == previousWindow.window_id,
          freshContext.origin_window_id == previousWindow.window_id,
          previousContext.origin_window_frame == previousWindow.frame,
          freshContext.origin_window_frame == previousWindow.frame,
          let process = currentProcess,
          process.seconds > 0, process.micros < 1_000_000,
          previousContext.process.start_seconds == process.seconds,
          previousContext.process.start_micros == process.micros,
          previous.read.structure.app.pid == fresh.read.structure.app.pid,
          previous.read.structure.app.bundle_id == fresh.read.structure.app.bundle_id,
          previous.read.structure.active_layer == previousWindow.window_id,
          fresh.read.structure.active_layer == previousWindow.window_id,
          previousWindows.count == 1, let previousSourceWindow = previousWindows.first,
          freshWindows.count == 1, let freshWindow = freshWindows.first,
          streamWindowMetadataMatches(previousSourceWindow, previousWindow),
          streamWindowMetadataMatches(freshWindow, previousWindow, allowZIndexChange: true),
          previousWindow.kind == "window", !previousWindow.modal,
          previousWindow.owner_bundle_id == previous.read.structure.app.bundle_id,
          freshWindow.kind == "window", !freshWindow.modal,
          freshWindow.owner_bundle_id == fresh.read.structure.app.bundle_id,
          streamHasSelectedStandardRoot(previous.read.structure, window: previousWindow),
          streamHasSelectedStandardRoot(fresh.read.structure, window: freshWindow),
          captureReadPrivacyEligible(previous.read, window: previousWindow),
          captureReadPrivacyEligible(fresh.read, window: freshWindow),
          previous.read.visiblePrivacy.map({ $0.processStamp == process }) ?? previous.read.structure.complete,
          fresh.read.visiblePrivacy.map({ $0.processStamp == process }) ?? fresh.read.structure.complete,
          let selectedID = UInt32(String(previousWindow.window_id.dropFirst("window-".count))),
          nativeWindows.filter({ $0.id == selectedID }).count == 1,
          let nativeWindow = nativeWindows.first(where: { $0.id == selectedID }),
          nativeWindow.pid == fresh.read.structure.app.pid,
          nativeWindow.frame == freshWindow.frame,
          nativeWindow.zIndex == freshWindow.z_index else { return nil }
    return freshWindow
}

private func streamWindowMetadataMatches(_ actual: WindowInfo, _ expected: WindowInfo,
                                         allowZIndexChange: Bool = false) -> Bool {
    actual.window_id == expected.window_id
        && actual.title == expected.title
        && actual.frame == expected.frame
        && (allowZIndexChange || actual.z_index == expected.z_index)
        && actual.kind == expected.kind
        && actual.modal == expected.modal
        && actual.owner_bundle_id == expected.owner_bundle_id
}

private func streamHasSelectedStandardRoot(_ structure: Structure, window: WindowInfo) -> Bool {
    let roots = structure.objects.filter { $0.window_id == window.window_id && $0.parent == nil }
    guard roots.count == 1, let root = roots.first else { return false }
    return root.role == "AXWindow" && root.subrole == "AXStandardWindow"
        && root.frame == window.frame && root.title == window.title && !root.secure
}

func streamRefreshSampleEligible(_ metadata: StreamSampleMetadata, boundary: UInt64, width: Int, height: Int) -> Bool {
    guard boundary > 0 else { return true }
    return metadata.status == "complete" && metadata.displayNs.map { $0 > boundary && $0 <= metadata.receivedNs } == true
        && metadata.width == width && metadata.height == height
}

func streamRefreshTransitionDiscardable(_ metadata: StreamSampleMetadata, boundary: UInt64, width: Int, height: Int) -> Bool {
    // Only known normal clocked transitions are discardable. Invalid clock
    // fields continue into the normal fatal timestamp validation path.
    guard boundary > 0, metadata.status == "complete", metadata.displayTicks.map({ $0 > 0 }) == true,
          let display = metadata.displayNs, display > 0,
          display <= metadata.receivedNs else { return false }
    return display <= boundary || (metadata.width == max(1, width / 2) && metadata.height == max(1, height / 2))
}

func streamRefreshBatchUnsafe(_ status: StreamSampleMetadata?, boundary: UInt64) -> Bool {
    boundary > 0 && ["blank", "suspended", "stopped"].contains(status?.status ?? "")
}

func streamInitialCompleteDeadlineExpired(nowNs: UInt64, baselineFinishedNs: UInt64,
                                          timeoutNs: UInt64) -> Bool {
    baselineFinishedNs > 0 && (nowNs < baselineFinishedNs || nowNs - baselineFinishedNs >= timeoutNs)
}

/// Failure diagnostics inspect only the already selected process/window's
/// metadata. The all-window lookup is never used to loosen the capture guard.
private func missingSelectedWindowReason(pid: UInt32, window: WindowInfo) -> String {
    guard let list = CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]],
          let id = UInt32(window.window_id.replacingOccurrences(of: "window-", with: "")),
          let entry = list.first(where: {
              ($0[kCGWindowOwnerPID as String] as? NSNumber)?.uint32Value == pid
              && ($0[kCGWindowNumber as String] as? NSNumber)?.uint32Value == id
          }) else { return "selected window disappeared from CG window metadata during processing" }
    let onScreen = (entry[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue ?? false
    let layer = (entry[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1
    return "selected window absent from on-screen capture geometry (on_screen=\(onScreen), layer=\(layer))"
}

private func exactFrameDescription(_ rect: Rect) -> String {
    "(x=\(rect.x),y=\(rect.y),w=\(rect.width),h=\(rect.height))"
}

private func streamObservationStructure(_ reading: AXSessionRead) -> Structure {
    var structure = reading.read.structure
    structure.identity_boundary = reading.identity_boundary
    if let context = reading.backgroundWindowContext {
        structure.background_window_context = NativeBackgroundWindowContext(
            window: WindowContext(source_session_id: context.source_session_id,
                session_epoch: context.session_epoch,
                process: KernelProcessIdentity(pid: context.process_id,
                    start_seconds: context.process.seconds, start_micros: context.process.micros),
                origin_window_id: context.window_id,
                origin_window_frame: context.frame,
                verified_source_sequence: 1,
                verified_observed_ms: reading.finished_unix_ms),
            hidden: .known(false), minimized: .known(false))
    } else {
        structure.background_window_context = nil
    }
    return structure
}

@MainActor
func watchSource(request: NativeRequest) async throws {
    guard let bundle = request.bundle_id, let outputDir = request.output_dir else {
        throw NativeFailure("watch requires target bundle_id and private output_dir")
    }
    guard let scope = request.watchScope else { throw NativeFailure("watch control scope unavailable") }
    let writer = StreamWriter(scope: scope)
    do {
        if request.background_target != nil {
            try await runBackgroundAXWatchSource(request: request, bundle: bundle,
                scope: scope, writer: writer)
        } else {
            try await runWatchSource(request: request, bundle: bundle, outputDir: outputDir,
                                     scope: scope, writer: writer)
        }
    } catch {
        try? writer.terminalUnavailable(boundedText(String(describing: error)))
        throw WatchTerminalFailure()
    }
}

@MainActor
private func runWatchSource(request: NativeRequest, bundle: String, outputDir: String,
                            scope: NativeWatchScope, writer: StreamWriter) async throws {
    let controlReader = try WatchControlReader(scope: scope)
    defer { controlReader.stop() }
    let apps = NSRunningApplication.runningApplications(withBundleIdentifier: bundle)
        .filter { !$0.isTerminated && (request.pid == nil || UInt32($0.processIdentifier) == request.pid) }
    guard apps.count == 1, let target = apps.first else {
        try writer.health(source: "ax", state: "unavailable", reason: "watch target missing or ambiguous")
        throw NativeFailure("watch target missing or ambiguous")
    }
    let app = AppIdentity(bundle_id: bundle, name: boundedText(target.localizedName ?? bundle), pid: UInt32(target.processIdentifier))
    let timer = SourceMain.watchdog(timeoutMS: request.timeout + request.interval + 1000)
    defer { timer.cancel() }
    let session = AXSession(app: app, request: request)
    let mailbox = StreamMailbox()
    let signals = stopSignals(mailbox: mailbox)
    defer { for source in signals { source.cancel() } }
    var current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
    var readings: [AXSessionRead] = [current]
    var hasPublishedPixels = false
    func remember(_ reading: AXSessionRead) {
        readings.append(reading)
        if readings.count > 4 { readings.removeFirst() }
    }
    var lastReadNs = current.read.finished
    var lastFrame: (id: String, metadata: StreamSampleMetadata)?
    var lastSeenDisplay: UInt64 = 0
    var lastFrameCallbackNs: UInt64 = 0
    var publishedGeometry: StreamGeometry?
    var assetNames: [String] = []
    var revision: UInt64 = 1
    var refreshAttempts = 0
    var refreshStartedNs: UInt64 = 0
    var initialPixelBaselineNs: UInt64 = 0
    var streamStarted: UInt64 = 0
    var modalMismatchRetired = false
    func observation(_ reading: AXSessionRead, frameRef: String?, ocr: Ocr, errors: [String]) throws {
        try SourceMain.emit(StreamObservationResponse(stream_id: writer.id, sequence: writer.next(),
            received_uptime_ns: reading.read.finished, received_unix_ms: reading.finished_unix_ms,
            permissions: SourceMain.preflight(), structure: streamObservationStructure(reading),
            structure_started_uptime_ns: reading.read.started, structure_finished_uptime_ns: reading.read.finished,
            structure_started_unix_ms: reading.started_unix_ms, structure_finished_unix_ms: reading.finished_unix_ms,
            frame_ref: frameRef, tree_revision: reading.tree_revision, observer_revision: reading.observer_revision,
            observer_available: reading.observer_root_available, observer_reason: reading.observer_reason, ocr: ocr, errors: errors))
    }
    func emitGap(_ reason: String, dropped: UInt64 = 0,
                 ns: UInt64 = uptimeNs(), wall: UInt64 = unixMs()) throws {
        // Before the first real pixel there is no frame context to revoke. A
        // startup failure is reported as unavailable health, not a synthetic
        // gap that would clear an otherwise trusted initial Source scope.
        guard hasPublishedPixels else { return }
        try writer.gap(reason, dropped: dropped, ns: ns, wall: wall)
    }
    try observation(current, frameRef: nil, ocr: Ocr(), errors: [])
    // A newly shown owned window or an Operator focus transition can briefly
    // make AXWindows empty or invalidate the worker's pre/post window check.
    // Keep those failed reads visible, then retry at most twice. A retry must
    // independently satisfy the original privacy, identity and window rules.
    for _ in 0..<2 where !streamReadPrivacyEligible(current.read) {
        let permissions = SourceMain.preflight()
        if mailbox.cancelled || !permissions.accessibility || !permissions.screen_capture
            || !current.read.secureFrames.isEmpty { break }
        try await Task.sleep(for: .milliseconds(50))
        current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
        remember(current)
        lastReadNs = current.read.finished
        try observation(current, frameRef: nil, ocr: Ocr(), errors: [])
    }
    let initialPermissions = SourceMain.preflight()
    guard var window = observationWindow(current.read.structure),
          captureReadPrivacyEligible(current.read, window: window),
          initialPermissions.accessibility, initialPermissions.screen_capture else {
        // Private text can withhold pixels while the window metadata remains
        // readable. Keep the AX worker alive so a later operator authorization
        // is based on a current window. No value or screenshot is promoted.
        let withheld = initialWatchFailure(current.read, permissions: initialPermissions)
        try writer.health(source: "screenshot", state: "unavailable", reason: withheld)
        while !mailbox.cancelled && SourceMain.preflight().accessibility {
            timer.schedule(deadline: .now() + .milliseconds(request.timeout + request.interval + 1000))
            try await Task.sleep(for: .milliseconds(request.interval))
            if mailbox.cancelled { break }
            current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
            try observation(current, frameRef: nil, ocr: Ocr(), errors: [withheld])
            try writer.health(source: "ax", state: current.read.structure.complete ? "available" : "degraded",
                              reason: current.read.structure.incomplete_reason,
                              ns: current.read.finished, wall: current.finished_unix_ms)
            try writer.health(source: "ax_observer", state: current.read.structure.observer_coverage.state.rawValue,
                              reason: current.observer_reason,
                              ns: current.read.finished, wall: current.finished_unix_ms)
            guard NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id else { break }
        }
        await session.close()
        return
    }
    func retireForActiveModalMismatch(_ reading: AXSessionRead,
                                      receivedNs: UInt64? = nil,
                                      receivedWall: UInt64? = nil) throws -> Bool {
        guard streamHasActiveModalCaptureMismatch(reading.read.structure, capturedWindowID: window.window_id) else {
            return false
        }
        let reason = "active modal layer differs from captured window; stream retired"
        modalMismatchRetired = true
        lastFrame = nil
        try emitGap(reason, ns: receivedNs ?? reading.read.finished,
                    wall: receivedWall ?? reading.finished_unix_ms)
        try observation(reading, frameRef: nil, ocr: Ocr(), errors: [reason])
        try writer.health(source: "screenshot", state: "unavailable", reason: reason,
                          ns: receivedNs ?? reading.read.finished,
                          wall: receivedWall ?? reading.finished_unix_ms)
        return true
    }
    let sink = StreamSink(mailbox: mailbox)
    let stream: SCStream
    let configuration: SCStreamConfiguration
    let filter: SCContentFilter
    let selectedWindow: SCWindow
    do { (stream, configuration, filter, selectedWindow) = try await prepareStream(app: app, window: window, request: request, sink: sink) }
    catch {
        await session.close()
        throw error
    }
    // Native titlebar children can finish materializing when ScreenCaptureKit
    // first enumerates the target. Establish a stable pre-start AX baseline,
    // then refresh it against a new read after SCStream starts and before the
    // first pixel can be published.
    do {
        for _ in 0..<3 {
            guard !mailbox.cancelled, !Task.isCancelled else {
                throw NativeFailure("watch cancelled during startup baseline")
            }
            let next = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
            let stable = unchangedPrivacyContext(before: current.read, after: next.read)
            current = next; lastReadNs = next.read.finished
            remember(next)
            if stable { break }
            try await Task.sleep(for: .milliseconds(100))
        }
        if try retireForActiveModalMismatch(current) {
            mailbox.cancel()
            await session.close()
            return
        }
        guard current.captureRootContinuous == true,
              captureReadPrivacyEligible(current.read, window: window),
              current.read.structure.windows.contains(where: { $0.window_id == window.window_id && $0.frame == window.frame }) else {
            throw NativeFailure("prepared stream AX privacy baseline unavailable")
        }
        try observation(current, frameRef: nil, ocr: Ocr(), errors: [])
        let baselineBeforeStream = current
        guard !mailbox.cancelled, !Task.isCancelled else {
            throw NativeFailure("watch cancelled before stream startup")
        }
        try await stream.startCapture()
        streamStarted = uptimeNs()
        // Discard callbacks queued before the post-start AX read, then do one
        // fresh, identity-bound read before accepting any pixels.
        mailbox.beginRefresh(boundary: streamStarted, width: configuration.width, height: configuration.height)
        guard !mailbox.cancelled, !Task.isCancelled else {
            throw NativeFailure("watch cancelled during startup baseline")
        }
        let fresh = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
        current = fresh
        remember(fresh)
        lastReadNs = fresh.read.finished
        let currentProcess = ProcessStamp.read(pid: app.pid)
        let freshNativeWindows = targetWindows(pid: app.pid)
        let postStartPermissions = SourceMain.preflight()
        guard let freshWindow = streamStartupBaselineWindow(previous: baselineBeforeStream,
                previousWindow: window, fresh: fresh, expectedSessionEpoch: scope.sessionEpoch,
                streamStartedNs: streamStarted, currentProcess: currentProcess,
                nativeWindows: freshNativeWindows),
              let selectedID = UInt32(String(window.window_id.dropFirst("window-".count))),
              selectedWindow.windowID == selectedID,
              selectedWindow.owningApplication?.processID == pid_t(app.pid),
              selectedWindow.owningApplication?.bundleIdentifier == app.bundle_id,
              Rect(selectedWindow.frame).approximatelyEquals(freshWindow.frame),
              NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
              postStartPermissions.accessibility, postStartPermissions.screen_capture,
              !mailbox.cancelled, !Task.isCancelled else {
            try observation(fresh, frameRef: nil, ocr: Ocr(), errors: ["post-start selected-window baseline unavailable"])
            throw NativeFailure("post-start selected-window baseline unavailable")
        }
        window = freshWindow
        try observation(fresh, frameRef: nil, ocr: Ocr(), errors: [])
        // Updating configuration can replace the independently audited auxiliary
        // root. Preserve the post-start baseline and wait for a real complete
        // callback displayed after it, without introducing another transition.
        // Queued samples from the AX read are discarded; idle cannot supply pixels.
        refreshStartedNs = fresh.read.finished
        initialPixelBaselineNs = fresh.read.finished
        mailbox.beginRefresh(boundary: refreshStartedNs,
                             width: configuration.width, height: configuration.height)
        try writer.health(source: "screenshot", state: "degraded",
                          reason: "waiting for new complete pixels after startup AX baseline",
                          ns: refreshStartedNs, wall: unixMs())
    } catch {
        mailbox.cancel()
        try? await stream.stopCapture()
        await session.close()
        throw error
    }
    do {
        while !mailbox.cancelled {
            timer.schedule(deadline: .now() + .milliseconds(request.timeout + request.interval + 1000))
            guard SourceMain.preflight().accessibility, SourceMain.preflight().screen_capture,
                  NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id else {
                try emitGap("watch permissions or app identity changed")
                try writer.health(source: "screenshot", state: "unavailable", reason: "watch permissions or app identity changed")
                break
            }
            // Idle callbacks attest transport activity, not new pixels. They
            // cannot extend the fixed first-complete deadline or a retry budget.
            if !hasPublishedPixels && streamInitialCompleteDeadlineExpired(nowNs: uptimeNs(),
                baselineFinishedNs: initialPixelBaselineNs,
                timeoutNs: UInt64(max(request.interval * 3, 3000)) * 1_000_000) {
                try writer.health(source: "screenshot", state: "unavailable",
                                  reason: "SCStream first complete pixel deadline exceeded")
                break
            }
            let batch = mailbox.drain()
            if let error = batch.4 {
                try emitGap(error)
                try writer.health(source: "screenshot", state: "unavailable", reason: error)
                break
            }
            if streamRefreshBatchUnsafe(batch.1, boundary: refreshStartedNs) {
                lastFrame = nil
                let reason = "SCStream unsafe refresh status \(batch.1?.status ?? "unknown")"
                try emitGap(reason, dropped: batch.2)
                try writer.health(source: "screenshot", state: "unavailable", reason: reason)
                // An unsafe co-batch precedes all positive publication and
                // queue recovery; none of its queued pixels are eligible.
                break
            }
            let activity = batch.3 == 0 ? streamStarted : batch.3
            if uptimeNs() - activity > UInt64(max(request.interval * 3, 3000)) * 1_000_000 {
                try emitGap("SCStream callback deadline exceeded")
                try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream callback deadline exceeded")
                break
            }
            if batch.2 > 0 {
                lastFrame = nil
                try emitGap("SCStream latest-frame queue replaced unprocessed pixels", dropped: batch.2)
                if refreshStartedNs > 0 {
                    // Discard the whole batch. A queued callback predates gap
                    // detection and cannot restore health, even at full size.
                    current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                    remember(current); lastReadNs = current.read.finished
                    if try retireForActiveModalMismatch(current) { break }
                    try observation(current, frameRef: nil, ocr: Ocr(), errors: ["refresh pixel queue gap"])
                    if current.captureRootContinuous == true
                        && streamRebaselineAllowed(current.read, window: window,
                        nativeWindows: targetWindows(pid: app.pid), refreshAttempts: refreshAttempts) {
                        refreshAttempts += 1; revision += 2
                        refreshStartedNs = uptimeNs()
                        let refreshStartedWall = unixMs()
                        let width = configuration.width; let height = configuration.height
                        mailbox.beginRefresh(boundary: refreshStartedNs, width: width, height: height)
                        configuration.width = max(1, width / 2); configuration.height = max(1, height / 2)
                        try await stream.updateConfiguration(configuration)
                        configuration.width = width; configuration.height = height
                        try await stream.updateConfiguration(configuration)
                        try writer.health(source: "screenshot", state: "degraded", reason: "waiting for new complete pixels after bounded refresh queue recovery",
                                          ns: refreshStartedNs, wall: refreshStartedWall)
                        continue
                    }
                }
                try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream pixel queue gap requires a new capture session")
                // The queued sample predates detecting the gap. Never restore
                // source health using that older callback's observation time.
                break
            }
            while let delivery = controlReader.mailbox.next(), !mailbox.cancelled {
                switch delivery {
                case .capture(let control):
                    defer { controlReader.mailbox.finish(control.requestID) }
                    guard let pixels = lastFrame?.metadata else {
                        try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                            nextSequence: { writer.next() }, code: .captureUnavailable))
                        continue
                    }
                    do {
                        if let result = try await captureSameWatch(control: control, scope: scope, app: app,
                            window: window, request: request, filter: filter, selectedWindow: selectedWindow,
                            configuration: configuration, pixelMetadata: pixels, geometryRevision: revision,
                            session: session, mailbox: controlReader.mailbox,
                            didRead: { reading in current = reading; remember(reading); lastReadNs = reading.read.finished },
                            nextSequence: { writer.next() }) {
                            // The async capture does not attest stream liveness.
                            guard !controlReader.mailbox.isCancelled(control.requestID),
                                  mailbox.captureAllowed(nowNS: uptimeNs(), timeoutNS: UInt64(max(request.interval * 3, 3000)) * 1_000_000) else {
                                if let frame = result.frame { removeCancelledCaptureAsset(frame.png_file, outputDir: outputDir) }
                                // Discarded results already reserved one stream record sequence.
                                // Emit a correlated failure using that sequence, without a gap.
                                let failure = CaptureResultResponse(stream_id: result.stream_id, sequence: result.sequence,
                                    received_uptime_ns: uptimeNs(), received_unix_ms: unixMs(),
                                    watch_id: scope.watchID, request_id: control.requestID, session_epoch: scope.sessionEpoch,
                                    surface: scope.surface, status: "failed", requested_after_ms: control.afterMS,
                                    deadline_ms: control.deadlineMS, capture_started_uptime_ns: nil, capture_finished_uptime_ns: nil,
                                    capture_started_unix_ms: nil, capture_finished_unix_ms: nil,
                                    image_frame_id: nil, frame: nil, observation: nil, code: NativeCaptureResultCode.captureUnavailable.rawValue)
                                try SourceMain.emit(failure)
                                continue
                            }
                            try SourceMain.emit(result)
                        }
                    } catch let failure as WatchCaptureFailure {
                        if !controlReader.mailbox.isCancelled(control.requestID) {
                            try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                                nextSequence: { writer.next() }, code: failure.code))
                        }
                    } catch {
                        if !controlReader.mailbox.isCancelled(control.requestID) {
                            try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                                nextSequence: { writer.next() }, code: .internalError))
                        }
                    }
                case .cancel(let requestID):
                    controlReader.mailbox.finish(requestID)
                case .inputClosed:
                    mailbox.cancel()
                case .rejected(let control, let code):
                    try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                        nextSequence: { writer.next() }, code: code))
                case .failed:
                    try emitGap("watch control input failed its bounded scope or concurrency contract")
                    try writer.health(source: "screenshot", state: "unavailable", reason: "watch control unavailable")
                    mailbox.cancel()
                }
            }
            if let sample = batch.0 {
                lastFrame = nil
                let metadata = sample.metadata
                if metadata.receivedNs < lastFrameCallbackNs {
                    try emitGap("queued complete callback precedes published frame status", ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try writer.health(source: "screenshot", state: "unavailable", reason: "queued complete callback chronology regressed")
                    break
                }
                guard let display = metadata.displayNs, display > lastSeenDisplay,
                      display <= metadata.receivedNs,
                      metadata.receivedNs - display <= UInt64(request.timeout) * 1_000_000 else {
                    try emitGap("SCStream complete timestamp stale, replayed or unmapped", ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream source clock invalid")
                    break
                }
                lastSeenDisplay = display
                // Configuration refresh can briefly deliver an intermediate
                // size. Only its restored, newly displayed pixels are eligible.
                if !streamRefreshSampleEligible(metadata, boundary: refreshStartedNs,
                                                width: configuration.width, height: configuration.height) {
                    if streamRefreshTransitionDiscardable(metadata, boundary: refreshStartedNs,
                        width: configuration.width, height: configuration.height) { continue }
                    try emitGap("SCStream unexpected refresh complete clock or geometry")
                    try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream unexpected refresh complete clock or geometry")
                    break
                }
                if let geometry = publishedGeometry, geometry != StreamGeometry(metadata) {
                    try emitGap("SCStream pixel geometry or scale changed", ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream geometry change requires a new capture session",
                                      ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    break
                }
                // Callback pixels can queue while AX/OCR processes the previous
                // frame. Choose a retained AX reading that actually preceded
                // this source display time; the newest reading may be later.
                guard let before = precedingAXRead(readings, displayNs: display) else {
                    try emitGap("no retained AX baseline precedes queued frame", ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try writer.health(source: "screenshot", state: "unavailable", reason: "queued frame lacks a preceding AX privacy baseline")
                    break
                }
                current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                remember(current)
                // Indicator creation or switching to the Operator can race a
                // global window pre/post check. Retry bounded complete reads;
                // the incomplete reading is never promoted into a fact.
                for _ in 0..<2 where !streamReadPrivacyEligible(current.read) {
                    if mailbox.cancelled { break }
                    try await Task.sleep(for: .milliseconds(50))
                    current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                    remember(current)
                }
                lastReadNs = current.read.finished
                if try retireForActiveModalMismatch(current, receivedNs: metadata.receivedNs,
                                                    receivedWall: metadata.receivedUnixMs) { break }
                let beforeWindow = capturedWindowRead(before.read, windowID: window.window_id)
                let afterWindow = capturedWindowRead(current.read, windowID: window.window_id)
                let valid = unchangedPrivacyContext(before: beforeWindow, after: afterWindow)
                    && before.captureRootContinuous == true
                    && current.captureRootContinuous == true
                    && before.read.finished <= metadata.receivedNs
                    && current.read.started >= metadata.receivedNs
                    && metadata.receivedNs - before.read.finished <= UInt64(request.timeout) * 1_000_000
                    && targetWindows(pid: app.pid).contains {
                        $0.ref == window.window_id && $0.frame == window.frame && $0.zIndex == window.z_index
                    }
                if !valid {
                    let reason = revalidationReason(before: beforeWindow, after: afterWindow, metadata: metadata, window: window, request: request)
                    try emitGap(reason, ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try observation(current, frameRef: nil, ocr: Ocr(), errors: [reason])
                    if current.captureRootContinuous == true
                        && streamRebaselineAllowed(current.read, window: window,
                        nativeWindows: targetWindows(pid: app.pid), refreshAttempts: refreshAttempts) {
                        refreshAttempts += 1; revision += 2
                        refreshStartedNs = uptimeNs()
                        let refreshStartedWall = unixMs()
                        let width = configuration.width; let height = configuration.height
                        mailbox.beginRefresh(boundary: refreshStartedNs, width: width, height: height)
                        configuration.width = max(1, width / 2); configuration.height = max(1, height / 2)
                        try await stream.updateConfiguration(configuration)
                        configuration.width = width; configuration.height = height
                        try await stream.updateConfiguration(configuration)
                        try writer.health(source: "screenshot", state: "degraded",
                                          reason: "waiting for genuine SCStream complete after bounded configuration refresh",
                                          ns: refreshStartedNs, wall: refreshStartedWall)
                        continue
                    }
                    try writer.health(source: "screenshot", state: "unavailable", reason: "stream AX revalidation failed; bounded refresh exhausted or privacy unsafe")
                    break
                } else if let image = sample.image() {
                    // One-window stream is configured without shadows. Surface
                    // points must be converted before checking for padding.
                    guard streamContentCoversPixels(metadata) else {
                        throw NativeFailure("SCStream content rectangle does not cover captured pixels")
                    }
                    let ocr = recognizeText(image: image, window: window.frame, maxBlocks: request.ocrLimit)
                    guard uptimeNs() - metadata.receivedNs <= UInt64(request.timeout) * 1_000_000 else {
                        throw NativeFailure("stream frame processing exceeded freshness budget")
                    }
                    current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                    remember(current)
                    for _ in 0..<2 where !streamReadPrivacyEligible(current.read) {
                        if mailbox.cancelled { break }
                        try await Task.sleep(for: .milliseconds(50))
                        current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                        remember(current)
                    }
                    lastReadNs = current.read.finished
                    if try retireForActiveModalMismatch(current, receivedNs: metadata.receivedNs,
                                                        receivedWall: metadata.receivedUnixMs) { break }
                    guard current.captureRootContinuous == true,
                          captureReadPrivacyEligible(current.read, window: window) else {
                        let reason = "AX privacy validation unavailable before frame publication"
                        lastFrame = nil
                        try emitGap(reason, ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                        try observation(current, frameRef: nil, ocr: Ocr(), errors: [reason])
                        try writer.health(source: "screenshot", state: "unavailable", reason: reason,
                                          ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                        break
                    }
                    let publicationWindow = capturedWindowRead(current.read, windowID: window.window_id)
                    guard unchangedPrivacyContext(before: afterWindow, after: publicationWindow) else {
                        let reason = "AX privacy context changed before frame publication"
                        lastFrame = nil
                        try emitGap(reason, ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                        try observation(current, frameRef: nil, ocr: Ocr(), errors: [reason])
                        try writer.health(source: "screenshot", state: "unavailable", reason: reason,
                                          ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                        break
                    }
                    let afterProcessing = SourceMain.preflight()
                    guard afterProcessing.accessibility else { throw NativeFailure("Accessibility permission unavailable after stream processing") }
                    guard afterProcessing.screen_capture else { throw NativeFailure("Screen Recording permission unavailable after stream processing") }
                    guard let actual = targetWindows(pid: app.pid).first(where: { $0.ref == window.window_id }) else {
                        throw NativeFailure(missingSelectedWindowReason(pid: app.pid, window: window))
                    }
                    guard actual.frame == window.frame else {
                        throw NativeFailure("selected window bounds changed during processing: expected \(exactFrameDescription(window.frame)); actual \(exactFrameDescription(actual.frame))")
                    }
                    guard actual.zIndex == window.z_index else {
                        throw NativeFailure("selected window stacking changed during processing")
                    }
                    if mailbox.cancelled { break }
                    let (file, bytes) = try savePNG(image, outputDir: outputDir)
                    let frameID = "frame-\(writer.id)-\(writer.sequence + 1)"
                    try SourceMain.emit(StreamFrameResponse(stream_id: writer.id, sequence: writer.next(),
                        received_uptime_ns: metadata.receivedNs, received_unix_ms: metadata.receivedUnixMs,
                        frame: streamFrame(metadata: metadata, window: window, app: app, revision: revision, id: frameID, bytes: bytes, file: file)))
                    hasPublishedPixels = true
                    lastFrameCallbackNs = metadata.receivedNs
                    publishedGeometry = StreamGeometry(metadata)
                    lastFrame = (frameID, metadata)
                    assetNames.append(file)
                    if assetNames.count > 8 {
                        let old = assetNames.removeFirst()
                        let dir = open(outputDir, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                        if dir >= 0 { _ = unlinkat(dir, old, 0); close(dir) }
                    }
                    try observation(current, frameRef: frameID, ocr: ocr, errors: [])
                    try writer.health(source: "screenshot", state: "available", reason: nil, ns: metadata.receivedNs, wall: metadata.receivedUnixMs)
                    try writer.health(source: "ocr", state: ocr.complete ? "available" : "degraded", reason: ocr.reason,
                                      ns: current.read.finished, wall: current.finished_unix_ms)
                    refreshStartedNs = 0
                    mailbox.endRefresh()
                } else { throw NativeFailure("SCStream owned pixel image invalid") }
            }
            // Configuration changes can deliver half-size idle metadata too.
            // No idle confirmation is published until new full COMPLETE pixels
            // have passed the refreshed AX baseline; unsafe statuses stay visible.
            if let status = batch.1, status.receivedNs >= lastFrameCallbackNs,
               !(refreshStartedNs > 0 && lastFrame == nil && ["idle", "started"].contains(status.status)) {
                if let geometry = publishedGeometry, geometry != StreamGeometry(status) {
                    try emitGap("SCStream idle/status geometry or scale changed", ns: status.receivedNs, wall: status.receivedUnixMs)
                    try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream geometry change requires a new capture session",
                                      ns: status.receivedNs, wall: status.receivedUnixMs)
                    break
                }
                let reference = status.status == "idle" ? lastFrame : nil
                if reference != nil {
                    current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                    remember(current)
                    for _ in 0..<2 where !streamReadPrivacyEligible(current.read) {
                        if mailbox.cancelled { break }
                        try await Task.sleep(for: .milliseconds(50))
                        current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                        remember(current)
                    }
                    lastReadNs = current.read.finished
                    if try retireForActiveModalMismatch(current, receivedNs: status.receivedNs,
                                                        receivedWall: status.receivedUnixMs) { break }
                    if current.captureRootContinuous != true
                        || !captureReadPrivacyEligible(current.read, window: window) {
                        let reason = "AX privacy validation unavailable before cached frame"
                        lastFrame = nil
                        try emitGap(reason, ns: status.receivedNs, wall: status.receivedUnixMs)
                        try observation(current, frameRef: nil, ocr: Ocr(), errors: [reason])
                        try writer.health(source: "screenshot", state: "unavailable", reason: reason,
                                          ns: status.receivedNs, wall: status.receivedUnixMs)
                        break
                    }
                }
                try SourceMain.emit(StreamFrameResponse(stream_id: writer.id, sequence: writer.next(),
                    received_uptime_ns: status.receivedNs, received_unix_ms: status.receivedUnixMs,
                    frame: streamFrame(metadata: status, window: window, app: app, revision: revision,
                                       id: reference?.id, pixelMetadata: reference?.metadata)))
                lastFrameCallbackNs = status.receivedNs
                if ["blank", "suspended", "stopped"].contains(status.status) {
                    lastFrame = nil
                    try writer.health(source: "screenshot", state: "unavailable", reason: "SCStream status \(status.status)",
                                      ns: status.receivedNs, wall: status.receivedUnixMs)
                    if status.status == "stopped" { break }
                }
            }
            if uptimeNs() - lastReadNs >= UInt64(request.interval) * 1_000_000 {
                current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                remember(current)
                for _ in 0..<2 where !streamReadPrivacyEligible(current.read) {
                    if mailbox.cancelled { break }
                    try await Task.sleep(for: .milliseconds(50))
                    current = await session.read(frontmostPID: NSWorkspace.shared.frontmostApplication.map { UInt32($0.processIdentifier) })
                    remember(current)
                }
                lastReadNs = current.read.finished
                if try retireForActiveModalMismatch(current) { break }
                try observation(current, frameRef: nil, ocr: Ocr(), errors: [])
                try writer.health(source: "ax", state: current.read.structure.complete ? "available" : "degraded",
                                  reason: current.read.structure.incomplete_reason,
                                  ns: current.read.finished, wall: current.finished_unix_ms)
                try writer.health(source: "ax_observer", state: current.read.structure.observer_coverage.state.rawValue,
                                  reason: current.observer_reason,
                                  ns: current.read.finished, wall: current.finished_unix_ms)
                if current.captureRootContinuous != true
                    || !captureReadPrivacyEligible(current.read, window: window)
                    || !current.read.structure.windows.contains(where: { $0.window_id == window.window_id && $0.frame == window.frame })
                    || !targetWindows(pid: app.pid).contains(where: {
                        $0.ref == window.window_id && $0.frame == window.frame && $0.zIndex == window.z_index
                    }) {
                    lastFrame = nil
                    try emitGap("AX periodic validation invalidated the captured surface")
                    try writer.health(source: "screenshot", state: "unavailable", reason: "AX selected window, privacy or geometry invalid")
                    break
                }
            }
            try await Task.sleep(for: .milliseconds(100))
        }
        try await stream.stopCapture()
        await session.close()
        try writer.health(source: "screenshot", state: "unavailable",
                          reason: modalMismatchRetired ? "active modal layer differs from captured window; stream retired" : "watch stopped")
    } catch {
        mailbox.cancel()
        try? await stream.stopCapture()
        await session.close()
        if hasPublishedPixels { try? writer.gap(boundedText(String(describing: error))) }
        throw error
    }
}

@MainActor
private func runBackgroundAXWatchSource(request: NativeRequest, bundle: String,
                                        scope: NativeWatchScope, writer: StreamWriter) async throws {
    guard let backgroundTarget = request.background_target,
          backgroundTarget.valid(bundle: request.bundle_id, pid: request.pid),
          request.table_scope == nil, request.text_policy == nil else {
        throw NativeFailure("background AX-only watch request is invalid")
    }
    let controlReader = try WatchControlReader(scope: scope)
    defer { controlReader.stop() }
    let apps = NSRunningApplication.runningApplications(withBundleIdentifier: bundle)
        .filter { !$0.isTerminated && (request.pid == nil || UInt32($0.processIdentifier) == request.pid) }
    guard apps.count == 1, let target = apps.first else {
        try writer.health(source: "ax", state: "unavailable", reason: "background AX target missing or ambiguous")
        throw NativeFailure("background AX target missing or ambiguous")
    }
    let app = AppIdentity(bundle_id: bundle, name: boundedText(target.localizedName ?? bundle),
                          pid: UInt32(target.processIdentifier))
    let session = AXSession(app: app, request: request)
    defer { Task { await session.close() } }

    func emitObservation(_ reading: AXSessionRead) throws {
        var ocr = Ocr()
        ocr.complete = false
        ocr.reason = "background AX-only Source does not capture OCR"
        let errors = reading.read.structure.complete ? [] : [reading.read.structure.incomplete_reason ?? "background AX read unavailable"]
        try SourceMain.emit(StreamObservationResponse(stream_id: writer.id, sequence: writer.next(),
            received_uptime_ns: reading.read.finished, received_unix_ms: reading.finished_unix_ms,
            permissions: SourceMain.preflight(), structure: streamObservationStructure(reading),
            structure_started_uptime_ns: reading.read.started, structure_finished_uptime_ns: reading.read.finished,
            structure_started_unix_ms: reading.started_unix_ms, structure_finished_unix_ms: reading.finished_unix_ms,
            frame_ref: nil, tree_revision: reading.tree_revision, observer_revision: reading.observer_revision,
            observer_available: reading.observer_root_available, observer_reason: reading.observer_reason,
            ocr: ocr, errors: errors))
        let axState = reading.read.structure.complete ? "available" : "degraded"
        try writer.health(source: "ax", state: axState,
            reason: reading.read.structure.incomplete_reason,
            ns: reading.read.finished, wall: reading.finished_unix_ms)
        try writer.health(source: "ax_observer", state: reading.read.structure.observer_coverage.state.rawValue,
            reason: reading.observer_reason,
            ns: reading.read.finished, wall: reading.finished_unix_ms)
        try writer.health(source: "screenshot", state: "unavailable",
            reason: "background AX-only Source does not capture pixels",
            ns: reading.read.finished, wall: reading.finished_unix_ms)
        try writer.health(source: "ocr", state: "unavailable",
            reason: ocr.reason, ns: reading.read.finished, wall: reading.finished_unix_ms)
    }

    var reading = await session.readBackgroundAXOnly()
    try emitObservation(reading)
    var nextReadNs = uptimeNs() + UInt64(request.interval) * 1_000_000
    while !Task.isCancelled {
        switch controlReader.mailbox.next() {
        case .capture(let control):
            try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                nextSequence: { writer.next() }, code: .captureUnavailable))
            controlReader.mailbox.finish(control.requestID)
        case .cancel(let requestID):
            controlReader.mailbox.finish(requestID)
        case .rejected(let control, let code):
            try SourceMain.emit(failedCaptureResult(control: control, scope: scope,
                nextSequence: { writer.next() }, code: code))
        case .failed:
            throw NativeFailure("background AX watch control channel failed")
        case .inputClosed:
            return
        case nil:
            break
        }
        if uptimeNs() >= nextReadNs {
            reading = await session.readBackgroundAXOnly()
            try emitObservation(reading)
            nextReadNs = uptimeNs() + UInt64(request.interval) * 1_000_000
            guard NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id else {
                throw NativeFailure("background AX target process exited during its watch")
            }
        }
        try await Task.sleep(for: .milliseconds(20))
    }
}

func runStreamTests() throws -> [String] {
    let terminalScope = NativeWatchScope(watchID: "watch-terminal-test", sessionEpoch: 7,
                                         surface: SurfaceRef(id: "main", epoch: 7))
    guard let terminal = streamTerminalResponse(scope: terminalScope, sequence: 4,
            receivedUptimeNS: 1_200, receivedUnixMS: 1_700_000_000_000,
            reason: "SCStream startup failed") else {
        throw NativeFailure("watch terminal record fixture invalid")
    }
    let terminalWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(terminal)) as? [String: Any]
    let terminalSurface = terminalWire?["surface"] as? [String: Any]
    let terminalSequence = (terminalWire?["sequence"] as? NSNumber)?.uint64Value
    let terminalEpoch = (terminalWire?["session_epoch"] as? NSNumber)?.uint64Value
    let terminalSurfaceEpoch = (terminalSurface?["epoch"] as? NSNumber)?.uint64Value
    try requireTest((terminalWire?["protocol_version"] as? NSNumber)?.intValue == 1
                    && terminalWire?["kind"] as? String == "health"
                    && terminalWire?["stream_id"] as? String == terminalScope.watchID
                    && terminalSequence == 4
                    && terminalWire?["source"] as? String == "screenshot"
                    && terminalWire?["state"] as? String == "unavailable"
                    && terminalWire?["reason"] as? String == "SCStream startup failed"
                    && (terminalWire?["received_uptime_ns"] as? NSNumber)?.uint64Value == 1_200
                    && (terminalWire?["received_unix_ms"] as? NSNumber)?.uint64Value == 1_700_000_000_000
                    && terminalEpoch == 7
                    && terminalSurface?["id"] as? String == "main"
                    && terminalSurfaceEpoch == 7
                    && terminalWire?.keys.sorted() == ["kind", "protocol_version", "received_unix_ms",
                        "received_uptime_ns", "reason", "sequence", "session_epoch", "source",
                        "state", "stream_id", "surface"].sorted(),
                    "terminal watch health retains stream, session, surface and a closed wire shape")
    try requireTest(streamTerminalResponse(scope: NativeWatchScope(watchID: "",
                        sessionEpoch: 7, surface: SurfaceRef(id: "main", epoch: 7)), sequence: 4,
                        receivedUptimeNS: 1_200, receivedUnixMS: 1_700_000_000_000,
                        reason: "mismatched scope") == nil
                    && streamTerminalResponse(scope: terminalScope, sequence: 0,
                        receivedUptimeNS: 1_200, receivedUnixMS: 1_700_000_000_000,
                        reason: "zero sequence") == nil
                    && streamTerminalResponse(scope: terminalScope, sequence: 5,
                        receivedUptimeNS: 1_200, receivedUnixMS: 1_700_000_000_000,
                        reason: " \n ") == nil,
                    "terminal watch health rejects mismatched scope, zero sequence and empty reason")
    try requireTest(!shouldEmitGenericErrorResponse(for: WatchTerminalFailure())
                    && shouldEmitGenericErrorResponse(for: NativeFailure("ordinary command failure")),
                    "only a watch with its own terminal record suppresses generic ErrorResponse")
    let content = Rect(CGRect(x: 0, y: 0, width: 2, height: 2))
    let metadata = StreamSampleMetadata(status: "complete", displayTicks: 1, displayNs: 100_000_000,
        receivedNs: 120_000_000, receivedUnixMs: 1000, width: 2, height: 2, scaleFactor: 1,
        contentScale: 1, contentRect: content, dirtyRects: [content])
    try requireTest(streamSourceUnixMs(metadata) == 980, "source clock uses original callback age")
    let fractional = StreamSampleMetadata(status: "complete", displayTicks: 1, displayNs: 99_999_999,
        receivedNs: 120_000_000, receivedUnixMs: 1000, width: 2, height: 2, scaleFactor: 1,
        contentScale: 1, contentRect: content, dirtyRects: [])
    try requireTest(streamSourceUnixMs(fractional) == 979, "fractional source age rounds conservatively")
    try requireTest(streamContentCoversPixels(metadata), "1x surface points match pixel dimensions")
    func geometry(_ rect: CGRect, scale: Double = 2) -> StreamSampleMetadata {
        StreamSampleMetadata(status: "complete", displayTicks: 1, displayNs: 100_000_000,
            receivedNs: 120_000_000, receivedUnixMs: 1000, width: 640, height: 328, scaleFactor: scale,
            contentScale: 0.5, contentRect: Rect(rect), dirtyRects: [])
    }
    try requireTest(streamContentCoversPixels(geometry(CGRect(x: 0, y: 0, width: 320, height: 164))),
                    "Retina surface points are converted to pixels")
    try requireTest(!streamContentCoversPixels(geometry(CGRect(x: 4, y: 0, width: 316, height: 164)))
                    && !streamContentCoversPixels(geometry(CGRect(x: 0, y: 0, width: 320, height: 150)))
                    && !streamContentCoversPixels(geometry(CGRect(x: 0, y: 0, width: 320, height: 164), scale: .nan)),
                    "unknown padding, aspect or scale does not authorize full-window OCR")
    let mailbox = StreamMailbox()
    let pixels = OwnedStreamSample(metadata: metadata, pixels: Data(repeating: 255, count: 16), bytesPerRow: 8)
    mailbox.offer(metadata, pixels: pixels)
    let idle = StreamSampleMetadata(status: "idle", displayTicks: 2, displayNs: 110_000_000,
        receivedNs: 130_000_000, receivedUnixMs: 1010, width: 2, height: 2, scaleFactor: 1,
        contentScale: 1, contentRect: content, dirtyRects: [])
    mailbox.offer(idle, pixels: nil)
    let batch = mailbox.drain()
    try requireTest(batch.0 != nil && batch.1?.status == "idle" && batch.2 == 0,
                    "idle callback preserves queued complete pixels")
    let window = WindowInfo(window_id: "window-1", title: nil, frame: content, z_index: 0,
                            kind: "window", modal: false, owner_bundle_id: "test.fixture")
    let app = AppIdentity(bundle_id: "test.fixture", name: "Fixture", pid: 1)
    let confirmation = streamFrame(metadata: idle, window: window, app: app, revision: 1,
                                   id: "original", pixelMetadata: metadata)
    try requireTest(confirmation.status == "idle" && confirmation.frame_id == "original"
                    && confirmation.display_time_uptime_ns == metadata.displayNs
                    && confirmation.source_unix_ms == streamSourceUnixMs(metadata)
                    && confirmation.byte_length == 0 && confirmation.png_file == nil,
                    "idle does not refresh pixel time or create assets")
    var structure = Structure(app: app)
    structure.complete = true; structure.incomplete_reason = nil
    structure.active_layer = window.window_id; structure.windows = [window]
    try requireTest(!streamHasActiveModalCaptureMismatch(structure, capturedWindowID: window.window_id),
                    "active captured parent window does not retire its own stream")
    var inactiveStructure = structure
    inactiveStructure.active_layer = nil
    try requireTest(!streamHasActiveModalCaptureMismatch(inactiveStructure, capturedWindowID: window.window_id),
                    "unknown foreground activity preserves the existing inactive capture policy")
    let sheet = WindowInfo(window_id: "window-2", title: nil, frame: content, z_index: 1,
                           kind: "sheet", modal: true, owner_bundle_id: app.bundle_id)
    var modalStructure = structure
    modalStructure.active_layer = sheet.window_id
    modalStructure.windows.append(sheet)
    try requireTest(streamHasActiveModalCaptureMismatch(modalStructure, capturedWindowID: window.window_id),
                    "a proven active modal sheet retires the parent-window stream")
    var unknownActiveLayer = structure
    unknownActiveLayer.active_layer = "window-unknown"
    try requireTest(!streamHasActiveModalCaptureMismatch(unknownActiveLayer, capturedWindowID: window.window_id),
                    "an unmapped active layer is not guessed to be a modal sheet")
    let unrelatedWindow = WindowInfo(window_id: "window-3", title: nil, frame: content, z_index: 2,
                                     kind: "window", modal: false, owner_bundle_id: app.bundle_id)
    var unrelatedActiveWindow = structure
    unrelatedActiveWindow.active_layer = unrelatedWindow.window_id
    unrelatedActiveWindow.windows.append(unrelatedWindow)
    try requireTest(!streamHasActiveModalCaptureMismatch(unrelatedActiveWindow, capturedWindowID: window.window_id),
                    "an unrelated nonmodal active window preserves existing capture policy")
    var incompleteModalStructure = modalStructure
    incompleteModalStructure.complete = false
    try requireTest(streamHasActiveModalCaptureMismatch(incompleteModalStructure, capturedWindowID: window.window_id)
                    && !incompleteModalStructure.complete,
                    "modal retirement does not upgrade an incomplete Source tree")
    let object = UiObject(instance_ref: "captured", parent: nil, window_id: window.window_id,
        role: "AXButton", subrole: nil, title: "safe", description: nil, value: nil,
        enabled: true, focused: false, frame: content, actions: [], in_view: true, group: nil, secure: false)
    structure.objects = [object]
    func reading(_ time: UInt64, secure: Bool = false,
                 identityBoundary: SourceIdentityBoundary? = nil) -> AXSessionRead {
        AXSessionRead(read: StructureRead(structure: structure, started: time, finished: time,
                                         secureFrames: secure ? [content] : []),
                      tree_revision: 1, observer_revision: 0, observer_available: false,
                      observer_reason: "offline", started_unix_ms: 1, finished_unix_ms: 1,
                      identity_boundary: identityBoundary)
    }
    let identityBoundary = SourceIdentityBoundary(namespace: "00112233-4455-6677-8899-aabbccddeeff", generation: 3)
    try requireTest(streamObservationStructure(reading(140)).identity_boundary == nil &&
                    streamObservationStructure(reading(140, identityBoundary: identityBoundary)).identity_boundary == identityBoundary,
                    "stream DTO carries only the actual AXSession identity boundary")
    let boundaryStructure = streamObservationStructure(reading(140, identityBoundary: identityBoundary))
    let boundaryWire = try JSONSerialization.jsonObject(with: JSONEncoder().encode(boundaryStructure)) as? [String: Any]
    let boundaryFields = boundaryWire?["identity_boundary"] as? [String: Any]
    try requireTest(boundaryFields?["namespace"] as? String == identityBoundary.namespace &&
                    boundaryFields?["generation"] as? Int == 3,
                    "stream DTO encodes the identity boundary beside its Source structure")
    let preceding = reading(50); let newer = reading(150)
    try requireTest(precedingAXRead([preceding, newer], displayNs: 100)?.read.finished == 50,
                    "queued frame selects AX reading before display, not latest")
    try requireTest(precedingAXRead([newer], displayNs: 100) == nil, "missing preceding AX read fails closed")
    let original = reading(50).read
    var beforeValue = structure
    beforeValue.objects.insert(UiObject(instance_ref: "root", parent: nil, window_id: window.window_id,
        role: "AXWindow", subrole: "AXStandardWindow", title: "Root", description: nil, value: nil,
        enabled: true, focused: true, frame: content, actions: [], in_view: true, group: nil, secure: false), at: 0)
    beforeValue.objects.append(UiObject(instance_ref: "value", parent: "root", window_id: window.window_id,
        role: "AXMenuButton", subrole: nil, title: "Menu", description: nil, value: .text("before"),
        enabled: true, focused: false, frame: content, actions: ["show_menu"], in_view: true, group: nil, secure: false))
    var afterValue = beforeValue
    afterValue.objects[2] = UiObject(instance_ref: "value", parent: "root", window_id: window.window_id,
        role: "AXMenuButton", subrole: nil, title: "Menu", description: nil, value: .text("selected"),
        enabled: true, focused: false, frame: content, actions: ["show_menu"], in_view: true, group: nil, secure: false)
    func recoveryRead(_ value: Structure, sensitive: Bool = false) -> StructureRead {
        StructureRead(structure: value, started: 140_000_000, finished: 150_000_000,
            secureFrames: sensitive ? [content] : [])
    }
    let native = [NativeWindow(id: 1, pid: app.pid, frame: content, zIndex: 0)]
    try requireTest(!unchangedPrivacyContext(before: recoveryRead(beforeValue), after: recoveryRead(afterValue))
                    && streamRebaselineAllowed(recoveryRead(afterValue), window: window, nativeWindows: native, refreshAttempts: 0)
                    && streamRebaselineAllowed(recoveryRead(afterValue), window: window, nativeWindows: native, refreshAttempts: 1)
                    && !streamRebaselineAllowed(recoveryRead(afterValue), window: window, nativeWindows: native, refreshAttempts: 2),
                    "post-publication actual value changes reject old pixels but permit at most two fresh baselines")
    for reason in ["AX read truncated", "unknown menu relationship", "AX geometry noValue"] {
        var partial = afterValue; partial.complete = false; partial.incomplete_reason = reason
        try requireTest(!streamRebaselineAllowed(recoveryRead(partial), window: window, nativeWindows: native, refreshAttempts: 0),
                        "partial or unknown privacy cannot acquire a replacement baseline")
    }
    var missingRoot = afterValue; missingRoot.objects.removeFirst()
    try requireTest(!streamRebaselineAllowed(recoveryRead(afterValue, sensitive: true), window: window, nativeWindows: native, refreshAttempts: 0)
                    && !streamRebaselineAllowed(recoveryRead(missingRoot), window: window, nativeWindows: native, refreshAttempts: 0)
                    && !streamRebaselineAllowed(recoveryRead(afterValue), window: window, nativeWindows: [], refreshAttempts: 0),
                    "sensitive, missing AX root or invalid CG window cannot refresh pixels")
    func refreshSample(status: String = "complete", display: UInt64 = 170_000_000, width: Int = 2) -> StreamSampleMetadata {
        StreamSampleMetadata(status: status, displayTicks: 3, displayNs: display, receivedNs: 180_000_000,
            receivedUnixMs: 1010, width: width, height: 2, scaleFactor: 1, contentScale: 1,
            contentRect: content, dirtyRects: [])
    }
    let boundary: UInt64 = 160_000_000
    try requireTest(streamRefreshSampleEligible(refreshSample(), boundary: boundary, width: 2, height: 2)
                    && !streamRefreshSampleEligible(metadata, boundary: boundary, width: 2, height: 2)
                    && !streamRefreshSampleEligible(refreshSample(display: boundary), boundary: boundary, width: 2, height: 2)
                    && !streamRefreshSampleEligible(refreshSample(display: 190_000_000), boundary: boundary, width: 2, height: 2)
                    && !streamRefreshSampleEligible(refreshSample(width: 1), boundary: boundary, width: 2, height: 2)
                    && !streamRefreshSampleEligible(refreshSample(status: "idle"), boundary: boundary, width: 2, height: 2),
                    "refresh accepts only actual full COMPLETE pixels after the new baseline boundary")
    let refreshMailbox = StreamMailbox()
    refreshMailbox.beginRefresh(boundary: boundary, width: 2, height: 2)
    let full = refreshSample()
    let freshPixels = OwnedStreamSample(metadata: full, pixels: Data(repeating: 255, count: 16), bytesPerRow: 8)
    refreshMailbox.offer(full, pixels: freshPixels)
    let half = StreamSampleMetadata(status: "complete", displayTicks: 3, displayNs: 170_000_000,
        receivedNs: 180_000_000, receivedUnixMs: 1010, width: 1, height: 1, scaleFactor: 1,
        contentScale: 0.5, contentRect: content, dirtyRects: [])
    refreshMailbox.offer(half, pixels: freshPixels)
    refreshMailbox.offer(refreshSample(display: boundary), pixels: freshPixels)
    let filtered = refreshMailbox.drain()
    try requireTest(filtered.0?.metadata.displayNs == full.displayNs && filtered.0?.metadata.width == 2 && filtered.2 == 0,
                    "refresh half-size and stale transitions cannot overwrite queued genuine full pixels")
    refreshMailbox.endRefresh()
    refreshMailbox.offer(full, pixels: freshPixels); refreshMailbox.offer(full, pixels: freshPixels)
    try requireTest(refreshMailbox.drain().2 == 1, "ordinary pixel replacements still produce a queue gap")
    for unsafe in ["blank", "suspended", "stopped"] {
        let queue = StreamMailbox(); queue.beginRefresh(boundary: boundary, width: 2, height: 2)
        queue.offer(full, pixels: freshPixels); queue.offer(full, pixels: freshPixels)
        queue.offer(refreshSample(status: unsafe), pixels: nil)
        queue.offer(refreshSample(status: "idle"), pixels: nil)
        queue.offer(full, pixels: freshPixels)
        let unsafeBatch = queue.drain()
        try requireTest(unsafeBatch.0 == nil && unsafeBatch.2 == 1
                        && streamRefreshBatchUnsafe(unsafeBatch.1, boundary: boundary),
                        "unsafe refresh co-batch precedes positive publication and gap recovery")
        let crossing = StreamMailbox(); crossing.beginRefresh(boundary: boundary, width: 2, height: 2)
        crossing.offer(refreshSample(status: unsafe), pixels: nil)
        crossing.endRefresh()
        crossing.offer(refreshSample(status: "idle"), pixels: nil)
        crossing.offer(full, pixels: freshPixels)
        let crossed = crossing.drain()
        try requireTest(crossed.0 == nil && crossed.1?.status == unsafe,
                        "unsafe pending callback remains latched across endRefresh and later positive callbacks")
    }
    for invalid in [refreshSample(display: 0), refreshSample(display: 190_000_000), StreamSampleMetadata(status: "complete", displayTicks: nil,
        displayNs: nil, receivedNs: 180_000_000, receivedUnixMs: 1010, width: 2, height: 2,
        scaleFactor: 1, contentScale: 1, contentRect: content, dirtyRects: [])] {
        let queue = StreamMailbox(); queue.beginRefresh(boundary: boundary, width: 2, height: 2)
        queue.offer(full, pixels: freshPixels); queue.offer(invalid, pixels: freshPixels)
        let invalidBatch = queue.drain()
        try requireTest(!streamRefreshTransitionDiscardable(invalid, boundary: boundary, width: 2, height: 2)
                        && invalidBatch.0 == nil && invalidBatch.4 != nil,
                        "invalid refresh clock remains a fatal visible failure rather than a discarded transition")
    }
    let badGeometry = StreamMailbox(); badGeometry.beginRefresh(boundary: boundary, width: 2, height: 2)
    badGeometry.offer(refreshSample(width: 1), pixels: freshPixels)
    try requireTest(!streamRefreshTransitionDiscardable(refreshSample(width: 1), boundary: boundary, width: 2, height: 2)
                    && badGeometry.drain().4 != nil, "fresh dimensions outside the exact requested half-size pair remain fatal")
    let restarted = StreamMailbox(); restarted.beginRefresh(boundary: boundary, width: 2, height: 2)
    restarted.offer(full, pixels: freshPixels); restarted.offer(full, pixels: freshPixels)
    let lost = restarted.drain()
    try requireTest(lost.2 == 1, "true pending full-frame replacement remains a gap")
    let newBoundary: UInt64 = 175_000_000
    restarted.beginRefresh(boundary: newBoundary, width: 2, height: 2)
    restarted.offer(full, pixels: freshPixels)
    try requireTest(restarted.drain().0 == nil, "gap-preceding queued pixels cannot restore health at the new boundary")
    let idleDeadline = StreamMailbox()
    idleDeadline.beginRefresh(boundary: newBoundary, width: 2, height: 2)
    idleDeadline.offer(refreshSample(status: "idle"), pixels: nil)
    try requireTest(idleDeadline.drain().0 == nil
                    && !streamInitialCompleteDeadlineExpired(nowNs: newBoundary + 19_999_999,
                        baselineFinishedNs: newBoundary, timeoutNs: 20_000_000)
                    && streamInitialCompleteDeadlineExpired(nowNs: newBoundary + 20_000_000,
                        baselineFinishedNs: newBoundary, timeoutNs: 20_000_000)
                    && streamInitialCompleteDeadlineExpired(nowNs: newBoundary - 1,
                        baselineFinishedNs: newBoundary, timeoutNs: 20_000_000),
                    "idle transport cannot supply pixels or extend the fixed first-complete deadline")
    var indicator = original
    indicator.structure.objects.append(UiObject(instance_ref: "indicator", parent: nil, window_id: "window-2",
        role: "AXButton", subrole: nil, title: "WindowSharingSessionButton", description: nil, value: nil,
        enabled: true, focused: false, frame: content, actions: [], in_view: true, group: nil, secure: false))
    try requireTest(unchangedPrivacyContext(before: capturedWindowRead(original, windowID: window.window_id),
                                           after: capturedWindowRead(indicator, windowID: window.window_id)),
                    "uncaptured sharing indicator does not change captured window content")
    let sensitiveIndicator = StructureRead(structure: indicator.structure, started: indicator.started,
                                           finished: indicator.finished, secureFrames: [content])
    try requireTest(!unchangedPrivacyContext(before: capturedWindowRead(original, windowID: window.window_id),
                                            after: capturedWindowRead(sensitiveIndicator, windowID: window.window_id)),
                    "window scoped comparison retains global sensitive fail closed")
    var background = original
    background.structure.active_layer = nil
    try requireTest(unchangedPrivacyContext(before: capturedWindowRead(original, windowID: window.window_id),
                                           after: capturedWindowRead(background, windowID: window.window_id))
                    && background.structure.active_layer == nil,
                    "readonly privacy comparison leaves actual inactive layer unknown")
    let standardRoot = UiObject(instance_ref: "standard", parent: nil, window_id: window.window_id,
        role: "AXWindow", subrole: "AXStandardWindow", title: nil, description: nil, value: nil,
        enabled: false, focused: false, frame: content, actions: [], in_view: true, group: nil, secure: false)
    background.structure.objects = [standardRoot]
    try requireTest(observationWindow(background.structure)?.window_id == window.window_id,
                    "inactive observation selects only one standard nonmodal window")
    let second = WindowInfo(window_id: "window-2", title: nil, frame: content, z_index: 1,
                            kind: "window", modal: false, owner_bundle_id: app.bundle_id)
    background.structure.windows.append(second)
    background.structure.objects.append(UiObject(instance_ref: "sharing-indicator", parent: nil, window_id: second.window_id,
        role: "AXWindow", subrole: "AXDialog", title: nil, description: nil, value: nil,
        enabled: false, focused: false, frame: content, actions: [], in_view: true, group: nil, secure: false))
    try requireTest(observationWindow(background.structure)?.window_id == window.window_id,
                    "uncaptured sharing dialog is not a second standard target")
    background.structure.objects.removeLast()
    background.structure.objects.append(UiObject(instance_ref: "standard-2", parent: nil, window_id: second.window_id,
        role: "AXWindow", subrole: "AXStandardWindow", title: nil, description: nil, value: nil,
        enabled: false, focused: false, frame: content, actions: [], in_view: true, group: nil, secure: false))
    try requireTest(observationWindow(background.structure) == nil, "ambiguous inactive windows fail closed")
    mailbox.offer(metadata, pixels: pixels); mailbox.offer(metadata, pixels: pixels)
    try requireTest(mailbox.drain().2 == 1, "latest frame replacement records a gap")
    try requireTest(pixels.image()?.width == 2, "owned BGRA image survives callback lifetime")
    let overflow = machTicksToNs(UInt64.max)
    var info = mach_timebase_info_data_t(); _ = mach_timebase_info(&info)
    try requireTest(info.numer <= info.denom || overflow == nil, "mach clock overflow rejected")
    let scopedRead = visiblePrivacySelfTestRead()
    let scopedWindow = scopedRead.structure.windows[0]
    let scopedNative = NativeWindow(id: 1, pid: scopedRead.structure.app.pid,
        frame: scopedWindow.frame, zIndex: 0)
    let scopedComparison = capturedWindowRead(scopedRead, windowID: scopedWindow.window_id)
    try requireTest(!scopedComparison.structure.complete && scopedComparison.visiblePrivacy == scopedRead.visiblePrivacy
                    && captureReadPrivacyEligible(scopedComparison, window: scopedWindow)
                    && unchangedPrivacyContext(before: scopedComparison,
                        after: capturedWindowRead(visiblePrivacySelfTestRead(identitySuffix: "new-read"),
                                                  windowID: scopedWindow.window_id))
                    && streamRebaselineAllowed(scopedRead, window: scopedWindow,
                                               nativeWindows: [scopedNative], refreshAttempts: 0),
                    "stream scoped comparison and rebaseline require current private coverage without promoting complete")
    var uncovered = scopedRead; uncovered.visiblePrivacy = nil
    try requireTest(!captureReadPrivacyEligible(uncovered, window: scopedWindow)
                    && !unchangedPrivacyContext(before: scopedComparison,
                                                after: capturedWindowRead(uncovered, windowID: scopedWindow.window_id))
                    && !streamRebaselineAllowed(uncovered, window: scopedWindow,
                                                nativeWindows: [scopedNative], refreshAttempts: 0)
                    && !streamRebaselineAllowed(scopedRead, window: scopedWindow,
                                                nativeWindows: [scopedNative], refreshAttempts: 2),
                    "stream scope census cannot retain pixels or bypass the bounded refresh limit without private coverage")
    var lostForeground = uncovered
    lostForeground.structure.active_layer = nil
    lostForeground.privacyRejection = .notFrontmost
    let rejectedComparison = capturedWindowRead(lostForeground, windowID: scopedWindow.window_id)
    let diagnostic = captureRevalidationPrivacySummary(before: scopedComparison,
        after: rejectedComparison, window: scopedWindow)
    try requireTest(diagnostic.contains("privacy_before=eligible,privacy_after=missing_coverage")
                    && diagnostic.contains("reader_after=not_frontmost")
                    && diagnostic.contains("layer_before=captured,layer_after=absent")
                    && rejectedComparison.privacyRejection == .notFrontmost,
                    "capture comparison preserves read-local rejection and identifies the failed side")
    try requireTest(!diagnostic.contains(scopedWindow.window_id)
                    && !diagnostic.contains(scopedWindow.title ?? "Offline")
                    && !diagnostic.contains(scopedRead.structure.app.bundle_id)
                    && !diagnostic.contains(scopedRead.structure.objects[0].instance_ref),
                    "capture reason tags do not expose target text or identities")
    var unchangedFacts = scopedRead
    unchangedFacts.privacyRejection = .readUnavailable
    try requireTest(captureReadPrivacyEligible(unchangedFacts, window: scopedWindow)
                    && unchangedPrivacyContext(before: scopedRead, after: unchangedFacts),
                    "diagnostic tags do not change capture authority independently of facts")
    let auxiliaryRead = visiblePrivacyAuxiliarySelfTestRead()
    let auxiliaryWindow = auxiliaryRead.structure.windows.first {
        $0.window_id == auxiliaryRead.visiblePrivacy?.windowID
    }!
    let auxiliaryComparison = capturedWindowRead(auxiliaryRead, windowID: auxiliaryWindow.window_id)
    try requireTest(auxiliaryComparison.structure.windows.count == 2
                    && auxiliaryComparison.structure.objects.map(\.instance_ref)
                        == auxiliaryRead.structure.objects.map(\.instance_ref)
                    && captureReadPrivacyEligible(auxiliaryComparison, window: auxiliaryWindow)
                    && unchangedPrivacyContext(before: auxiliaryComparison,
                        after: capturedWindowRead(visiblePrivacyAuxiliarySelfTestRead(),
                                                  windowID: auxiliaryWindow.window_id)),
                    "partial capture comparison retains both audited roots without discarding the original inventory")
    for changedAuxiliary in [
        visiblePrivacyAuxiliarySelfTestRead(auxiliaryTitle: "Changed auxiliary"),
        visiblePrivacyAuxiliarySelfTestRead(auxiliaryValue: .text("Changed overlay value")),
        visiblePrivacyAuxiliarySelfTestRead(auxiliaryFrame: Rect(CGRect(x: -519, y: 500, width: 180, height: 140))),
    ] {
        let changedComparison = capturedWindowRead(changedAuxiliary, windowID: auxiliaryWindow.window_id)
        try requireTest(captureReadPrivacyEligible(changedComparison, window: auxiliaryWindow)
                        && !unchangedPrivacyContext(before: auxiliaryComparison, after: changedComparison),
                        "auxiliary title, value or geometry changes reject pixels across individually valid readings")
    }
    func startupSessionRead(_ base: StructureRead, zIndex: UInt32,
                            started: UInt64, finished: UInt64,
                            proofStarted: UInt64, proofFinished: UInt64,
                            sessionID: String = "startup-session",
                            epoch: UInt64 = 7,
                            process: ProcessStamp = ProcessStamp(seconds: 1, micros: 0),
                            identityGeneration: UInt64 = 1,
                            rootContinuous: Bool? = true,
                            title: String? = nil) -> AXSessionRead {
        var structure = base.structure
        structure.windows = structure.windows.map { item in
            let itemTitle = item.window_id == "window-1" ? (title ?? item.title) : item.title
            return WindowInfo(window_id: item.window_id, title: itemTitle, frame: item.frame,
                z_index: item.window_id == "window-1" ? zIndex : item.z_index,
                kind: item.kind, modal: item.modal, owner_bundle_id: item.owner_bundle_id)
        }
        if let title {
            structure.objects = structure.objects.map { object in
                guard object.window_id == "window-1", object.parent == nil else { return object }
                return UiObject(instance_ref: object.instance_ref, parent: object.parent,
                    window_id: object.window_id, role: object.role, subrole: object.subrole,
                    title: title, description: object.description, value: object.value,
                    enabled: object.enabled, focused: object.focused, frame: object.frame,
                    actions: object.actions, in_view: object.in_view, group: object.group,
                    secure: object.secure, identity_continuity: object.identity_continuity,
                    scroll: object.scroll)
            }
        }
        structure.window_context = WindowContext(source_session_id: sessionID, session_epoch: epoch,
            process: KernelProcessIdentity(pid: structure.app.pid, start_seconds: process.seconds,
                                           start_micros: process.micros),
            origin_window_id: "window-1", origin_window_frame: structure.windows[0].frame,
            verified_source_sequence: 0, verified_observed_ms: 0)
        let coverage = base.visiblePrivacy.map { old in
            VisiblePrivacyCoverage(bundleID: old.bundleID, processID: old.processID,
                processStamp: process, windowID: old.windowID, windowFrame: old.windowFrame,
                tableScope: old.tableScope, readStartedNS: proofStarted, readFinishedNS: proofFinished,
                visibleNodeCount: old.visibleNodeCount, visibleTreeFingerprint: old.visibleTreeFingerprint,
                tableViewport: old.tableViewport, auxiliaryWindows: old.auxiliaryWindows)
        }
        let read = StructureRead(structure: structure, started: started, finished: finished,
            secureFrames: base.secureFrames, visiblePrivacy: coverage, privacyRejection: base.privacyRejection)
        return AXSessionRead(read: read, tree_revision: 1, observer_revision: 0,
            observer_available: false, observer_reason: "offline", started_unix_ms: 1, finished_unix_ms: 1,
            identity_boundary: SourceIdentityBoundary(namespace: sessionID, generation: identityGeneration),
            captureRootContinuous: rootContinuous)
    }
    let startupProcess = ProcessStamp(seconds: 1, micros: 0)
    let startupBefore = startupSessionRead(visiblePrivacySelfTestRead(), zIndex: 11,
        started: 10, finished: 20, proofStarted: 12, proofFinished: 19, identityGeneration: 1)
    let startupFresh = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49, identityGeneration: 2)
    let startupNative = [NativeWindow(id: 1, pid: 1,
        frame: startupFresh.read.structure.windows[0].frame, zIndex: 12)]
    let startupWindow = startupBefore.read.structure.windows[0]
    let rebound = streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
        fresh: startupFresh, expectedSessionEpoch: 7, streamStartedNs: 30,
        currentProcess: startupProcess, nativeWindows: startupNative)
    try requireTest(rebound?.z_index == 12
                    && startupBefore.read.structure.windows.count == 1
                    && startupFresh.read.structure.windows.count == 2
                    && captureReadPrivacyEligible(startupFresh.read, window: startupFresh.read.structure.windows[0]),
                    "pre-pixel startup baseline accepts the same audited window after z-slot and auxiliary-root changes")
    let brokenRoot = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49,
        identityGeneration: 2, rootContinuous: false)
    let brokenPreviousRoot = startupSessionRead(visiblePrivacySelfTestRead(), zIndex: 11,
        started: 10, finished: 20, proofStarted: 12, proofFinished: 19,
        identityGeneration: 1, rootContinuous: false)
    let otherSession = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49,
        sessionID: "other-session", identityGeneration: 2)
    let wrongEpoch = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49,
        epoch: 8, identityGeneration: 2)
    let regressedGeneration = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49, identityGeneration: 0)
    let changedTitle = startupSessionRead(visiblePrivacyAuxiliarySelfTestRead(), zIndex: 12,
        started: 31, finished: 50, proofStarted: 32, proofFinished: 49,
        identityGeneration: 2, title: "Changed")
    try requireTest(streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: brokenRoot, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: brokenPreviousRoot, previousWindow: startupWindow,
                        fresh: startupFresh, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: otherSession, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: wrongEpoch, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: regressedGeneration, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: changedTitle, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: startupFresh, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: ProcessStamp(seconds: 2, micros: 0), nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: startupFresh, expectedSessionEpoch: 7, streamStartedNs: 31,
                        currentProcess: startupProcess, nativeWindows: startupNative) == nil
                    && streamStartupBaselineWindow(previous: startupBefore, previousWindow: startupWindow,
                        fresh: startupFresh, expectedSessionEpoch: 7, streamStartedNs: 30,
                        currentProcess: startupProcess,
                        nativeWindows: [NativeWindow(id: 1, pid: 1,
                            frame: startupFresh.read.structure.windows[0].frame, zIndex: 11)]) == nil,
                    "startup baseline rejects selected AX-root, session, process, metadata, freshness and CG-window changes")
    return ["stream_terminal_health_is_scoped_and_closed",
            "stream_terminal_health_rejects_invalid_identity_and_sequence",
            "stream_terminal_failure_suppresses_only_generic_error_response",
            "stream_revalidation_failed_side_diagnostic", "stream_revalidation_diagnostic_redacted",
            "stream_revalidation_diagnostic_not_authority",
            "stream_partial_auxiliary_inventory_preserved", "stream_auxiliary_cross_read_changes_rejected",
            "stream_post_start_baseline_rebinds_only_same_registered_window",
            "stream_post_start_baseline_rejects_changed_identity_or_expired_proof",
            "stream_scoped_private_coverage_preserved", "stream_scope_census_is_not_pixel_authority",
            "stream_original_callback_clock", "stream_identity_boundary_dto",
            "stream_fractional_source_age_conservative",
            "stream_content_points_1x", "stream_content_points_retina", "stream_content_padding_rejected", "stream_idle_preserves_pending_pixels",
            "stream_idle_preserves_pixel_time",
            "stream_queued_frame_preceding_ax_baseline", "stream_missing_preceding_ax_rejected",
            "stream_uncaptured_indicator_scoped", "stream_global_sensitive_evidence_retained",
            "stream_readonly_input_activity_separate", "stream_inactive_unique_standard_window",
            "stream_inactive_sharing_dialog_excluded",
            "stream_inactive_ambiguous_windows_rejected", "stream_modal_parent_capture_retained",
            "stream_modal_inactive_capture_policy_preserved",
            "stream_modal_mismatch_retires", "stream_modal_active_unknown_not_inferred",
            "stream_modal_unrelated_window_policy_preserved", "stream_modal_partial_not_promoted",
            "stream_latest_frame_gap", "stream_owned_bgra_image", "stream_mach_time_overflow",
            "stream_post_publication_value_change_rebaseline_bounded", "stream_partial_unknown_rebaseline_rejected",
            "stream_sensitive_missing_root_window_rebaseline_rejected", "stream_refresh_requires_new_full_complete_pixels",
            "stream_initial_complete_deadline_independent_of_idle_callbacks"]
        + ["stream_refresh_filters_transitions_before_queue_replacement", "stream_ordinary_queue_gap_preserved",
           "stream_refresh_unsafe_cobatch_not_hidden", "stream_refresh_invalid_clock_fatal",
           "stream_pending_full_queue_gap_preserved", "stream_gap_recovery_requires_later_native_pixels",
           "stream_pending_unsafe_survives_end_refresh_until_drain"]
}
