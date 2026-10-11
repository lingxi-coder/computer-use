import AppKit
import ScreenCaptureKit
import Darwin
import Foundation

struct WatchCaptureFailure: Error, Sendable {
    let code: NativeCaptureResultCode
}

private struct CaptureClockBounds {
    let lowerMS: UInt64
    let upperMS: UInt64
}

private func captureClockBounds(unixMS: UInt64, originUnixMS: UInt64) -> CaptureClockBounds? {
    guard unixMS >= originUnixMS else { return nil }
    let elapsed = unixMS - originUnixMS
    let lower = elapsed > 2 ? elapsed - 2 : 0
    let upper = elapsed.addingReportingOverflow(2)
    guard !upper.overflow else { return nil }
    return CaptureClockBounds(lowerMS: lower, upperMS: upper.partialValue)
}

private func withinTwoMilliseconds(_ lhs: UInt64, _ rhs: UInt64) -> Bool {
    lhs >= rhs ? lhs - rhs <= 2 : rhs - lhs <= 2
}

private func captureClockAnchorMatches(sampleUptimeNS: UInt64, sampleUnixMS: UInt64,
                                       headerUptimeNS: UInt64, headerUnixMS: UInt64) -> Bool {
    guard headerUptimeNS >= sampleUptimeNS, headerUnixMS >= sampleUnixMS else { return false }
    let uptimeDeltaMS = (headerUptimeNS - sampleUptimeNS) / 1_000_000
    return withinTwoMilliseconds(headerUnixMS - sampleUnixMS, uptimeDeltaMS)
}

private func validSelectedAXWindow(_ reading: AXSessionRead, app: AppIdentity,
                                  window: WindowInfo) -> Bool {
    let structure = reading.read.structure
    guard captureReadPrivacyEligible(reading.read, window: window),
          structure.app.pid == app.pid, structure.app.bundle_id == app.bundle_id,
          structure.active_layer == window.window_id else { return false }
    let matchingWindows = structure.windows.filter { $0.window_id == window.window_id }
    guard matchingWindows.count == 1, let current = matchingWindows.first,
          current.frame == window.frame, !current.modal, current.kind == "window",
          current.owner_bundle_id == app.bundle_id else { return false }
    return structure.objects.contains {
        $0.window_id == window.window_id && $0.parent == nil
            && $0.role == kAXWindowRole && $0.subrole == kAXStandardWindowSubrole
            && $0.frame == window.frame
    }
}

@MainActor
private func selectedSurfaceStillCurrent(app: AppIdentity, window: WindowInfo,
                                         selectedWindow: SCWindow, filter: SCContentFilter) async -> Bool {
    guard let rawWindowID = UInt32(window.window_id.replacingOccurrences(of: "window-", with: "")),
          selectedWindow.windowID == rawWindowID,
          selectedWindow.owningApplication?.processID == pid_t(app.pid),
          selectedWindow.owningApplication?.bundleIdentifier == app.bundle_id,
          Rect(selectedWindow.frame) == window.frame else { return false }
    let content: SCShareableContent
    do { content = try await SCShareableContent.excludingDesktopWindows(true, onScreenWindowsOnly: true) }
    catch { return false }
    // The selected object and its filter remain the actual capture target.
    // Fresh enumeration confirms the same unique visible descriptor, but its
    // public fields do not prove WindowServer ABA/incarnation continuity.
    let currentMatches = content.windows.filter {
        $0.windowID == rawWindowID
            && $0.owningApplication?.processID == pid_t(app.pid)
            && $0.owningApplication?.bundleIdentifier == app.bundle_id
    }
    guard currentMatches.count == 1, Rect(currentMatches[0].frame) == window.frame else { return false }
    if #available(macOS 15.2, *) {
        guard filter.includedWindows.count == 1,
              filter.includedWindows.first?.windowID == rawWindowID,
              filter.includedWindows.first?.owningApplication?.processID == pid_t(app.pid),
              filter.includedWindows.first?.owningApplication?.bundleIdentifier == app.bundle_id,
              filter.includedWindows.first.map({ Rect($0.frame) == window.frame }) == true else { return false }
    }
    let metadata = targetWindows(pid: app.pid).filter { $0.ref == window.window_id }
    return metadata.count == 1 && metadata[0].pid == app.pid && metadata[0].frame == window.frame
}

private func capturePixelGeometryValid(window: WindowInfo, configuration: SCStreamConfiguration,
                                       metadata: StreamSampleMetadata, image: CGImage? = nil) -> Bool {
    guard window.frame.valid, window.frame.width > 0, window.frame.height > 0,
          metadata.status == "complete", metadata.displayTicks.map({ $0 > 0 }) == true,
          metadata.displayNs.map({ $0 > 0 && $0 <= metadata.receivedNs }) == true,
          metadata.width > 0, metadata.width <= 2048, metadata.height > 0, metadata.height <= 1536,
          metadata.width == configuration.width, metadata.height == configuration.height,
          metadata.scaleFactor.isFinite, (1...4).contains(metadata.scaleFactor),
          metadata.contentScale.isFinite, metadata.contentScale > 0,
          streamContentCoversPixels(metadata),
          metadata.contentRect.x == 0, metadata.contentRect.y == 0,
          abs((metadata.contentRect.width - window.frame.width * metadata.contentScale) * metadata.scaleFactor) <= 1,
          abs((metadata.contentRect.height - window.frame.height * metadata.contentScale) * metadata.scaleFactor) <= 1,
          abs(Double(metadata.width) - window.frame.width * metadata.scaleFactor * metadata.contentScale) <= 1,
          abs(Double(metadata.height) - window.frame.height * metadata.scaleFactor * metadata.contentScale) <= 1 else { return false }
    guard let image else { return true }
    guard image.width == metadata.width, image.height == metadata.height,
          image.width > 0, image.height > 0,
          image.width <= 2048, image.height <= 1536 else { return false }
    // Retain the original SCK density and content scaling separately. A 2x
    // Retina window downsampled by 0.5 has one output pixel per window point.
    // The checks above establish the same full-window transform for both axes.
    return true
}

func runWatchCaptureGeometryTests() throws -> [String] {
    func valid(windowWidth: Double = 1160, windowHeight: Double = 768,
               content: Rect = Rect(CGRect(x: 0, y: 0, width: 580, height: 384)),
               density: Double = 2, contentScale: Double = 0.5) -> Bool {
        let window = WindowInfo(window_id: "window-1", title: nil,
            frame: Rect(CGRect(x: 176, y: 82, width: windowWidth, height: windowHeight)),
            z_index: 0, kind: "window", modal: false, owner_bundle_id: "test.offline")
        let configuration = SCStreamConfiguration()
        configuration.width = 1160
        configuration.height = 768
        let metadata = StreamSampleMetadata(status: "complete", displayTicks: 1, displayNs: 100,
            receivedNs: 200, receivedUnixMs: 1000, width: 1160, height: 768,
            scaleFactor: density, contentScale: contentScale, contentRect: content, dirtyRects: [])
        return capturePixelGeometryValid(window: window, configuration: configuration, metadata: metadata)
    }
    try requireTest(valid(), "same-watch capture retains Retina density and downsample scaling")
    try requireTest(valid(windowWidth: 580, windowHeight: 384, contentScale: 1),
                    "same-watch capture accepts unscaled Retina output")
    try requireTest(valid(content: Rect(CGRect(x: 0, y: 0, width: 1160, height: 768)),
                          density: 1, contentScale: 1), "same-watch capture accepts 1x output")
    try requireTest(!valid(contentScale: 2) && !valid(density: 1) && !valid(density: .nan)
                    && !valid(contentScale: .infinity) && !valid(contentScale: 0),
                    "same-watch capture rejects unrelated or unknown scale metadata")
    try requireTest(!valid(content: Rect(CGRect(x: 1, y: 0, width: 579, height: 384)))
                    && !valid(content: Rect(CGRect(x: 0, y: 0, width: 580, height: 380)))
                    && !valid(windowHeight: 772),
                    "same-watch capture rejects padding and anisotropic transforms")
    return ["same_watch_capture_retina_full_window_transform",
            "same_watch_capture_rejects_padding_and_inconsistent_scales"]
}

private func captureClockIsFresh(control: NativeCaptureNowControl, originUnixMS: UInt64,
                                 startedUnixMS: UInt64, finishedUnixMS: UInt64) -> Bool {
    guard let start = captureClockBounds(unixMS: startedUnixMS, originUnixMS: originUnixMS),
          let finish = captureClockBounds(unixMS: finishedUnixMS, originUnixMS: originUnixMS) else { return false }
    return start.lowerMS > control.afterMS && finish.upperMS >= start.lowerMS
        && finish.upperMS <= control.deadlineMS
}

@MainActor
private func waitForCaptureBarrier(_ control: NativeCaptureNowControl, originUnixMS: UInt64,
                                   mailbox: WatchControlMailbox) async throws -> Bool {
    while !mailbox.isCancelled(control.requestID) && !Task.isCancelled {
        let nowBounds = captureClockBounds(unixMS: unixMs(), originUnixMS: originUnixMS)
        guard let nowBounds else { throw WatchCaptureFailure(code: .captureUnavailable) }
        guard nowBounds.upperMS <= control.deadlineMS else {
            throw WatchCaptureFailure(code: .deadlineExpired)
        }
        if nowBounds.lowerMS > control.afterMS { return true }
        let remaining = control.afterMS - nowBounds.lowerMS
        let sleepMS = min(50, max(1, remaining + 3))
        do { try await Task.sleep(for: .milliseconds(Int(sleepMS))) }
        catch { return false }
    }
    return false
}

/// Remove a capture image if cancellation wins after saving but before the
/// watch loop publishes its response. The argument must be a basename emitted
/// by `savePNG`; this function never follows a path supplied by a caller.
func removeCancelledCaptureAsset(_ fileName: String, outputDir: String) {
    guard fileName.range(of: "^capture-[0-9a-f-]{36}\\.png$", options: .regularExpression) != nil,
          outputDir.hasPrefix("/"), outputDir.utf8.count <= 4096 else { return }
    let directory = open(outputDir, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
    guard directory >= 0 else { return }
    defer { close(directory) }
    var info = stat()
    guard fstat(directory, &info) == 0, info.st_uid == getuid(), info.st_mode & 0o777 == 0o700 else { return }
    _ = unlinkat(directory, fileName, 0)
}

@MainActor
func failedCaptureResult(control: NativeCaptureNowControl, scope: NativeWatchScope,
                         nextSequence: @MainActor () -> UInt64,
                         code: NativeCaptureResultCode) -> CaptureResultResponse {
    failedCaptureResult(control: control, scope: scope, sequence: nextSequence(),
        receivedUptimeNS: uptimeNs(), receivedUnixMS: unixMs(), code: code)
}

@MainActor
func failedCaptureResult(control: NativeCaptureNowControl, scope: NativeWatchScope,
                         sequence: UInt64, receivedUptimeNS: UInt64? = nil,
                         receivedUnixMS: UInt64? = nil,
                         code: NativeCaptureResultCode) -> CaptureResultResponse {
    CaptureResultResponse(stream_id: scope.watchID, sequence: sequence,
        received_uptime_ns: receivedUptimeNS ?? uptimeNs(), received_unix_ms: receivedUnixMS ?? unixMs(),
        watch_id: scope.watchID, request_id: control.requestID, session_epoch: scope.sessionEpoch,
        surface: scope.surface, status: "failed", requested_after_ms: control.afterMS,
        deadline_ms: control.deadlineMS, capture_started_uptime_ns: nil,
        capture_finished_uptime_ns: nil, capture_started_unix_ms: nil, capture_finished_unix_ms: nil,
        image_frame_id: nil, frame: nil, observation: nil, code: code.rawValue)
}

/// Capture one image through the same retained filter and SCWindow selected
/// when this watch started. Cancellation returns nil; ordinary failures return
/// a bounded `capture_result` without leaking partial image or AX evidence.
@MainActor
private func captureSameWatchAttempt(control: NativeCaptureNowControl, scope: NativeWatchScope,
                                     app: AppIdentity, window: WindowInfo, request: NativeRequest,
                                     filter: SCContentFilter, selectedWindow: SCWindow,
                                     configuration: SCStreamConfiguration, pixelMetadata: StreamSampleMetadata,
                                     geometryRevision: UInt64, session: AXSession,
                                     mailbox: WatchControlMailbox,
                                     didRead: @MainActor (AXSessionRead) -> Void,
                                     nextSequence: @MainActor () -> UInt64) async throws -> CaptureResultResponse? {
    if mailbox.isCancelled(control.requestID) || Task.isCancelled { return nil }
    guard control.scope == scope, request.watchScope == scope, safeNativeWatchScope(scope),
          let originUnixMS = request.clock_origin_unix_ms,
          request.bundle_id == app.bundle_id, request.pid == nil || request.pid == app.pid,
          window.window_id.hasPrefix("window-"), window.kind == "window", !window.modal,
          window.owner_bundle_id == app.bundle_id, window.frame.valid,
          window.frame.width > 0, window.frame.height > 0 else {
        throw WatchCaptureFailure(code: .scopeMismatch)
    }
    guard capturePixelGeometryValid(window: window, configuration: configuration, metadata: pixelMetadata),
          let outputDir = request.output_dir, geometryRevision > 0 else {
        throw WatchCaptureFailure(code: .captureUnavailable)
    }

    guard try await waitForCaptureBarrier(control, originUnixMS: originUnixMS, mailbox: mailbox) else { return nil }
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
    let beforeRead = await session.read(frontmostPID: app.pid)
    didRead(beforeRead)
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
    guard validSelectedAXWindow(beforeRead, app: app, window: window) else {
        throw WatchCaptureFailure(code: .pairIncomplete)
    }
    let beforePermissions = SourceMain.preflight()
    guard beforePermissions.accessibility, beforePermissions.screen_capture else {
        throw WatchCaptureFailure(code: .permissionDenied)
    }
    guard NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
          NSRunningApplication(processIdentifier: pid_t(app.pid))?.isTerminated == false,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid),
          await selectedSurfaceStillCurrent(app: app, window: window, selectedWindow: selectedWindow, filter: filter) else {
        throw WatchCaptureFailure(code: .scopeMismatch)
    }
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }

    let captureStartedUptimeNS = uptimeNs()
    let captureStartedUnixMS = unixMs()
    let callClockBounds = captureClockBounds(unixMS: unixMs(), originUnixMS: originUnixMS)
    guard captureClockIsFresh(control: control, originUnixMS: originUnixMS,
                              startedUnixMS: captureStartedUnixMS, finishedUnixMS: captureStartedUnixMS),
          let callClockBounds, callClockBounds.upperMS <= control.deadlineMS else {
        throw WatchCaptureFailure(code: .deadlineExpired)
    }

    let image: CGImage
    do { image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration) }
    catch {
        if mailbox.isCancelled(control.requestID) || Task.isCancelled { return nil }
        throw WatchCaptureFailure(code: .captureUnavailable)
    }
    let captureFinishedUptimeNS = uptimeNs()
    let captureFinishedUnixMS = unixMs()
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
    guard captureFinishedUptimeNS >= captureStartedUptimeNS,
          captureFinishedUptimeNS - captureStartedUptimeNS <= UInt64(request.timeout) * 1_000_000 else {
        throw WatchCaptureFailure(code: .captureUnavailable)
    }
    guard captureClockIsFresh(control: control, originUnixMS: originUnixMS,
                              startedUnixMS: captureStartedUnixMS, finishedUnixMS: captureFinishedUnixMS) else {
        throw WatchCaptureFailure(code: .deadlineExpired)
    }
    guard capturePixelGeometryValid(window: window, configuration: configuration,
                                    metadata: pixelMetadata, image: image) else {
        throw WatchCaptureFailure(code: .captureUnavailable)
    }
    guard captureClockAnchorMatches(sampleUptimeNS: captureStartedUptimeNS, sampleUnixMS: captureStartedUnixMS,
                                    headerUptimeNS: captureFinishedUptimeNS, headerUnixMS: captureFinishedUnixMS) else {
        throw WatchCaptureFailure(code: .captureUnavailable)
    }

    let afterPermissions = SourceMain.preflight()
    guard afterPermissions.accessibility, afterPermissions.screen_capture else {
        throw WatchCaptureFailure(code: .permissionDenied)
    }
    guard NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
          NSRunningApplication(processIdentifier: pid_t(app.pid))?.isTerminated == false,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid),
          await selectedSurfaceStillCurrent(app: app, window: window, selectedWindow: selectedWindow, filter: filter) else {
        throw WatchCaptureFailure(code: .scopeMismatch)
    }
    let afterRead = await session.read(frontmostPID: app.pid)
    didRead(afterRead)
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
    guard beforeRead.read.finished <= captureStartedUptimeNS,
          afterRead.read.started >= captureFinishedUptimeNS,
          validSelectedAXWindow(afterRead, app: app, window: window),
          unchangedPrivacyContext(before: beforeRead.read, after: afterRead.read) else {
        throw WatchCaptureFailure(code: .pairIncomplete)
    }
    let postReadPermissions = SourceMain.preflight()
    guard postReadPermissions.accessibility, postReadPermissions.screen_capture else {
        throw WatchCaptureFailure(code: .permissionDenied)
    }
    guard NSRunningApplication(processIdentifier: pid_t(app.pid))?.bundleIdentifier == app.bundle_id,
          NSWorkspace.shared.frontmostApplication?.processIdentifier == pid_t(app.pid),
          await selectedSurfaceStillCurrent(app: app, window: window, selectedWindow: selectedWindow, filter: filter) else {
        throw WatchCaptureFailure(code: .scopeMismatch)
    }

    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
    let preSaveUnix = unixMs()
    guard let preSaveBounds = captureClockBounds(unixMS: preSaveUnix, originUnixMS: originUnixMS),
          preSaveBounds.upperMS <= control.deadlineMS else {
        throw WatchCaptureFailure(code: .deadlineExpired)
    }

    let (pngFile, byteLength): (String, Int)
    do { (pngFile, byteLength) = try savePNG(image, outputDir: outputDir) }
    catch { throw WatchCaptureFailure(code: .captureUnavailable) }
    guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else {
        removeCancelledCaptureAsset(pngFile, outputDir: outputDir)
        return nil
    }
    let imageFrameID = "capture-\(UUID().uuidString.lowercased())"
    let eventFrameID = "capture-event-\(UUID().uuidString.lowercased())"
    let sequence = nextSequence()
    let receivedUptimeNS = uptimeNs()
    let receivedUnixMS = unixMs()
    guard let savedClock = captureClockBounds(unixMS: receivedUnixMS, originUnixMS: originUnixMS),
          savedClock.upperMS <= control.deadlineMS else {
        removeCancelledCaptureAsset(pngFile, outputDir: outputDir)
        return failedCaptureResult(control: control, scope: scope, sequence: sequence,
            receivedUptimeNS: receivedUptimeNS, receivedUnixMS: receivedUnixMS, code: .deadlineExpired)
    }
    guard captureClockAnchorMatches(sampleUptimeNS: captureStartedUptimeNS, sampleUnixMS: captureStartedUnixMS,
                                    headerUptimeNS: receivedUptimeNS, headerUnixMS: receivedUnixMS),
          captureClockAnchorMatches(sampleUptimeNS: captureFinishedUptimeNS, sampleUnixMS: captureFinishedUnixMS,
                                    headerUptimeNS: receivedUptimeNS, headerUnixMS: receivedUnixMS) else {
        removeCancelledCaptureAsset(pngFile, outputDir: outputDir)
        return failedCaptureResult(control: control, scope: scope, sequence: sequence,
            receivedUptimeNS: receivedUptimeNS, receivedUnixMS: receivedUnixMS, code: .captureUnavailable)
    }
    var structure = afterRead.read.structure
    structure.identity_boundary = afterRead.identity_boundary
    structure.normalizeSourceProvenance(sequence: sequence, observedMS: afterRead.finished_unix_ms)
    let observation = StreamObservationResponse(stream_id: scope.watchID, sequence: sequence,
        received_uptime_ns: afterRead.read.finished, received_unix_ms: afterRead.finished_unix_ms,
        permissions: postReadPermissions, structure: structure,
        structure_started_uptime_ns: afterRead.read.started, structure_finished_uptime_ns: afterRead.read.finished,
        structure_started_unix_ms: afterRead.started_unix_ms, structure_finished_unix_ms: afterRead.finished_unix_ms,
        frame_ref: imageFrameID, tree_revision: afterRead.tree_revision,
        observer_revision: afterRead.observer_revision, observer_available: afterRead.observer_root_available,
        observer_reason: afterRead.observer_reason, ocr: Ocr(), errors: [])
    let frame = CaptureFrame(geometry_revision: geometryRevision, width_px: image.width, height_px: image.height,
        scale_factor: pixelMetadata.scaleFactor, content_scale: pixelMetadata.contentScale,
        content_rect: pixelMetadata.contentRect, window_id: window.window_id, pid: app.pid,
        frame: window.frame, byte_length: byteLength, png_file: pngFile, frame_id: eventFrameID)
    let response = CaptureResultResponse(stream_id: scope.watchID, sequence: sequence,
        received_uptime_ns: receivedUptimeNS, received_unix_ms: receivedUnixMS,
        watch_id: scope.watchID, request_id: control.requestID, session_epoch: scope.sessionEpoch,
        surface: scope.surface, status: "complete", requested_after_ms: control.afterMS,
        deadline_ms: control.deadlineMS, capture_started_uptime_ns: captureStartedUptimeNS,
        capture_finished_uptime_ns: captureFinishedUptimeNS,
        capture_started_unix_ms: captureStartedUnixMS, capture_finished_unix_ms: captureFinishedUnixMS,
        image_frame_id: imageFrameID, frame: frame, observation: observation, code: nil)
    return response
}

@MainActor
func captureSameWatch(control: NativeCaptureNowControl, scope: NativeWatchScope,
                      app: AppIdentity, window: WindowInfo, request: NativeRequest,
                      filter: SCContentFilter, selectedWindow: SCWindow,
                      configuration: SCStreamConfiguration, pixelMetadata: StreamSampleMetadata,
                      geometryRevision: UInt64, session: AXSession, mailbox: WatchControlMailbox,
                      didRead: @MainActor (AXSessionRead) -> Void,
                      nextSequence: @MainActor () -> UInt64) async throws -> CaptureResultResponse? {
    do {
        return try await captureSameWatchAttempt(control: control, scope: scope, app: app, window: window,
            request: request, filter: filter, selectedWindow: selectedWindow,
            configuration: configuration, pixelMetadata: pixelMetadata,
            geometryRevision: geometryRevision, session: session, mailbox: mailbox,
            didRead: didRead,
            nextSequence: nextSequence)
    } catch let failure as WatchCaptureFailure {
        guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
        return failedCaptureResult(control: control, scope: scope, nextSequence: nextSequence, code: failure.code)
    } catch {
        guard !mailbox.isCancelled(control.requestID), !Task.isCancelled else { return nil }
        return failedCaptureResult(control: control, scope: scope, nextSequence: nextSequence, code: .internalError)
    }
}
