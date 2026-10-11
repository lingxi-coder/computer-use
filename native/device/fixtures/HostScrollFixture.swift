import AppKit
import os

@MainActor
private func emitHostScrollFixture(_ value: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: value) else { return }
    data.append(10)
    FileHandle.standardOutput.write(data)
}

@MainActor
final class HostScrollWindow: NSWindow {
    override func accessibilityTitle() -> String? { title }
}

@MainActor
final class HostScrollGroup: NSView {
    var ordered: [NSView] = []
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { "Scroll fixture group" }
    override func accessibilityChildren() -> [Any]? { ordered.filter { !$0.isHidden } }
}

@MainActor
final class HostScrollDocument: NSView {
    override var isFlipped: Bool { true }
    override func isAccessibilityElement() -> Bool { false }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.textBackgroundColor.setFill()
        dirtyRect.fill()
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.monospacedSystemFont(ofSize: 15, weight: .regular),
            .foregroundColor: NSColor.labelColor
        ]
        for index in 0..<36 {
            let y = CGFloat(index * 32 + 12)
            // Leave the destination's actual control unobscured by row text.
            if (392...456).contains(Int(y)) { continue }
            if dirtyRect.intersects(NSRect(x: 12, y: y, width: 430, height: 26)) {
                ("Row \(index + 1) — native host scroll fixture" as NSString)
                    .draw(at: NSPoint(x: 12, y: y), withAttributes: attributes)
            }
        }
    }
}

@MainActor
final class HostScrollDestination: NSButton {
    var actionCalls = 0
    var pointerDownCalls = 0
    var pointerUpCalls = 0
    nonisolated let valueCalls = OSAllocatedUnfairLock(initialState: 0)
    nonisolated let pressCalls = OSAllocatedUnfairLock(initialState: 0)
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .checkBox }
    override func accessibilityTitle() -> String? { "MVP scroll destination" }
    override func isAccessibilityEnabled() -> Bool { isEnabled }
    override func accessibilityValue() -> Any? { NSNumber(value: state.rawValue) }
    override func accessibilityChildren() -> [Any]? { [] }
    override func accessibilityFrame() -> NSRect {
        guard let window else { return .zero }
        // This is the real control's screen geometry even while it is outside
        // the clip viewport. Source owns the independent visibility decision.
        return window.convertToScreen(convert(bounds, to: nil))
    }
    // Keep NSButton's real value/setter/press behavior and checkbox state.
    override func setAccessibilityValue(_ accessibilityValue: Any?) {
        valueCalls.withLock { $0 += 1 }
        super.setAccessibilityValue(accessibilityValue)
    }
    override func accessibilityPerformPress() -> Bool {
        pressCalls.withLock { $0 += 1 }
        return super.accessibilityPerformPress()
    }
    override func mouseDown(with event: NSEvent) {
        pointerDownCalls += 1
        super.mouseDown(with: event)
    }
    override func mouseUp(with event: NSEvent) {
        pointerUpCalls += 1
        super.mouseUp(with: event)
    }
    @objc func activated() { actionCalls += 1 }
}

@MainActor
final class HostScrollNavigation: NSButton {
    nonisolated let calls = OSAllocatedUnfairLock(initialState: 0)
    var onPress: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .radioButton }
    override func accessibilityTitle() -> String? { title }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    nonisolated override func accessibilityPerformPress() -> Bool {
        DispatchQueue.main.async { self.activated() }
        return true
    }
    @objc func activated() {
        calls.withLock { $0 += 1 }
        onPress?()
    }
}

@MainActor
final class HostScrollView: NSScrollView {
    weak var destination: HostScrollDestination?
    var scrollCalls = 0
    var events: [[String: Any]] = []
    var afterScroll: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .scrollArea }
    override func accessibilityTitle() -> String? { "MVP scroll document" }
    override func isAccessibilityEnabled() -> Bool { true }
    override func accessibilityChildren() -> [Any]? {
        var children: [Any] = []
        if let destination, !destination.isHidden { children.append(destination) }
        // Expose the actual AppKit scrollers. Their AX values and any absent
        // min/max attributes remain native, rather than synthetic metrics.
        if let verticalScroller { children.append(verticalScroller) }
        if let horizontalScroller { children.append(horizontalScroller) }
        return children
    }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    override func reflectScrolledClipView(_ clipView: NSClipView) {
        super.reflectScrolledClipView(clipView)
        if clipView === contentView { afterScroll?() }
    }
    override func scrollWheel(with event: NSEvent) {
        scrollCalls += 1
        let record: [String: Any] = [
            "type": event.type.rawValue,
            "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
            "timestamp": event.timestamp, "flags": event.cgEvent?.flags.rawValue ?? 0,
            "dx": event.scrollingDeltaX, "dy": event.scrollingDeltaY,
            "precise": event.hasPreciseScrollingDeltas, "phase": event.phase.rawValue,
            "momentum_phase": event.momentumPhase.rawValue,
            "raw_dx": event.cgEvent?.getIntegerValueField(.scrollWheelEventPointDeltaAxis2) ?? 0,
            "raw_dy": event.cgEvent?.getIntegerValueField(.scrollWheelEventPointDeltaAxis1) ?? 0
        ]
        events.append(record)
        if events.count > 128 { events.removeFirst() }
        // Only ordinary AppKit handling supplies the scroll effect. Neither
        // instrumentation nor the stdin control channel changes clip bounds.
        super.scrollWheel(with: event)
        afterScroll?()
        var notice = record
        notice["kind"] = "scroll_event"
        notice["offset_x"] = contentView.bounds.origin.x
        notice["offset_y"] = contentView.bounds.origin.y
        emitHostScrollFixture(notice)
    }
}

/// Package with bundle ID com.lingxi.computer-user.guard-fixture. The native
/// host obtains all target, viewport, page and checkbox evidence through AX.
@MainActor
final class HostScrollFixture: NSObject, NSApplicationDelegate {
    let window = HostScrollWindow(contentRect: NSRect(x: 240, y: 220, width: 520, height: 340),
                                  styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let group = HostScrollGroup(frame: NSRect(x: 0, y: 0, width: 520, height: 340))
    let scroll = HostScrollView(frame: NSRect(x: 20, y: 58, width: 480, height: 240))
    let document = HostScrollDocument(frame: NSRect(x: 0, y: 0, width: 460, height: 1200))
    let destination = HostScrollDestination(title: "MVP scroll destination", target: nil, action: nil)
    let documentNavigation = HostScrollNavigation(title: "Document", target: nil, action: nil)
    let otherNavigation = HostScrollNavigation(title: "Other", target: nil, action: nil)
    let pageLabel = NSTextField(labelWithString: "Page: Document")
    let offsetLabel = NSTextField(labelWithString: "Scroll offset x=0 y=0")
    var page = "Document"
    var occluder: NSWindow?
    var eventMonitor: Any?
    var inputEvents: [[String: Any]] = []
    var rawScrollCalls = 0

    func show() {
        window.title = "Computer Use Host Scroll Fixture — Document"
        window.isReleasedWhenClosed = false
        window.acceptsMouseMovedEvents = true
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .canJoinAllApplications]
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = false
        scroll.scrollerStyle = .legacy
        scroll.borderType = .bezelBorder
        scroll.documentView = document
        destination.frame = NSRect(x: 20, y: 420, width: 320, height: 28)
        destination.setButtonType(.switch)
        destination.state = .off
        destination.isEnabled = true
        destination.target = destination
        destination.action = #selector(HostScrollDestination.activated)
        document.addSubview(destination)
        destination.setAccessibilityParent(scroll)
        scroll.destination = destination
        scroll.verticalScroller?.setAccessibilityParent(scroll)
        offsetLabel.frame = NSRect(x: 20, y: 18, width: 480, height: 24)
        pageLabel.frame = NSRect(x: 330, y: 308, width: 170, height: 24)
        for (navigation, x) in [(documentNavigation, 20.0), (otherNavigation, 180.0)] {
            navigation.frame = NSRect(x: x, y: 306, width: 140, height: 28)
            navigation.setButtonType(.radio)
            navigation.bezelStyle = .rounded
            navigation.target = navigation
            navigation.action = #selector(HostScrollNavigation.activated)
        }
        for view in [documentNavigation, otherNavigation, pageLabel, scroll, offsetLabel] {
            group.addSubview(view)
            view.setAccessibilityParent(group)
        }
        group.ordered = [documentNavigation, otherNavigation, pageLabel, scroll, offsetLabel]
        group.setAccessibilityParent(window)
        window.contentView?.addSubview(group)
        documentNavigation.state = .on
        otherNavigation.state = .off
        documentNavigation.onPress = { [weak self] in self?.setPage(to: "Document") }
        otherNavigation.onPress = { [weak self] in self?.setPage(to: "Other") }
        scroll.afterScroll = { [weak self] in self?.updateOffsetLabel() }
        eventMonitor = NSEvent.addLocalMonitorForEvents(matching: [
            .mouseMoved, .scrollWheel, .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp,
            .otherMouseDown, .otherMouseUp, .keyDown, .keyUp, .flagsChanged
        ]) { [weak self] event in
            MainActor.assumeIsolated {
                guard let self else { return }
                if event.type == .scrollWheel { self.rawScrollCalls += 1 }
                // Unfiltered: ambient input and marker-zero events remain
                // visible to the independent driver alongside owned input.
                self.inputEvents.append([
                    "type": event.type.rawValue, "timestamp": event.timestamp,
                    "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
                    "flags": event.cgEvent?.flags.rawValue ?? 0
                ])
                if self.inputEvents.count > 256 { self.inputEvents.removeFirst() }
            }
            return event
        }
        focusWindow()
        updateOffsetLabel()
        emitHostScrollFixture([
            "kind": "ready", "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": "com.lingxi.computer-user.guard-fixture", "window_id": window.windowNumber,
            "role": "AXScrollArea", "label": "MVP scroll document",
            "scroll_frame": screenFrame(scroll), "destination_frame": screenFrame(destination),
            "viewport_frame": screenFrame(scroll.contentView)
        ])
        Thread { [self] in
            while let command = readLine(), command.utf8.count < 64 {
                guard ["stats", "focus-window", "quit", "occlude", "restore"].contains(command) else { break }
                DispatchQueue.main.async { self.control(command) }
                if command == "quit" { return }
            }
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
        }.start()
    }

    func screenFrame(_ view: NSView) -> [String: CGFloat] {
        let frame = view.window?.convertToScreen(view.convert(view.bounds, to: nil)) ?? .zero
        let height = CGDisplayBounds(CGMainDisplayID()).height
        return ["x": frame.minX, "y": height - frame.maxY, "width": frame.width, "height": frame.height]
    }

    func focusWindow() {
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    func updateOffsetLabel() {
        let point = scroll.contentView.bounds.origin
        offsetLabel.stringValue = "Scroll offset x=\(Int(point.x.rounded())) y=\(Int(point.y.rounded()))"
    }

    func setPage(to destinationPage: String) {
        page = destinationPage
        pageLabel.stringValue = "Page: \(page)"
        window.title = "Computer Use Host Scroll Fixture — \(page)"
        documentNavigation.state = page == "Document" ? .on : .off
        otherNavigation.state = page == "Other" ? .on : .off
        scroll.isHidden = page != "Document"
        destination.isHidden = page != "Document"
        offsetLabel.isHidden = page != "Document"
        // Hiding and revealing the same scroll view preserves real clip
        // bounds and the actual checkbox state across Document -> Other -> Document.
        emitHostScrollFixture(stats(kind: "navigation"))
    }

    func stats(kind: String) -> [String: Any] {
        let point = scroll.contentView.bounds.origin
        return [
            "kind": kind, "pid": ProcessInfo.processInfo.processIdentifier,
            "window_id": window.windowNumber, "page": page,
            "page_label": pageLabel.stringValue,
            "navigation_calls": documentNavigation.calls.withLock { $0 } + otherNavigation.calls.withLock { $0 },
            "document_navigation_calls": documentNavigation.calls.withLock { $0 },
            "other_navigation_calls": otherNavigation.calls.withLock { $0 },
            "value": destination.state == .on, "destination_value": destination.state == .on,
            "destination_action_calls": destination.actionCalls,
            "value_calls": destination.valueCalls.withLock { $0 },
            "press_calls": destination.pressCalls.withLock { $0 },
            "pointer_down_calls": destination.pointerDownCalls,
            "pointer_up_calls": destination.pointerUpCalls,
            "raw_scroll_calls": rawScrollCalls, "scroll_calls": scroll.scrollCalls,
            "input_events": inputEvents, "scroll_events": scroll.events,
            "offset_x": point.x, "offset_y": point.y, "visible_offset": offsetLabel.stringValue,
            "viewport_height": scroll.contentView.bounds.height,
            "document_height": document.bounds.height,
            "scroll_frame": screenFrame(scroll), "destination_frame": screenFrame(destination),
            "viewport_frame": screenFrame(scroll.contentView),
            "scrollbar_values": [
                "horizontal": scroll.horizontalScroller?.doubleValue as Any? ?? NSNull(),
                "vertical": scroll.verticalScroller?.doubleValue as Any? ?? NSNull()
            ],
            "window_visible": window.isVisible, "window_key": window.isKeyWindow,
            "window_miniaturized": window.isMiniaturized,
            "app_active": NSApplication.shared.isActive, "app_hidden": NSApplication.shared.isHidden,
            "app_window_ids": NSApplication.shared.windows.map(\.windowNumber),
            "app_ax_window_count": NSApplication.shared.accessibilityWindows()?.count ?? -1,
            "window_ax_element": window.isAccessibilityElement(), "window_ax_hidden": window.isAccessibilityHidden()
        ]
    }

    func control(_ command: String) {
        switch command {
        case "quit": NSApplication.shared.terminate(nil); return
        case "focus-window": focusWindow()
        case "occlude":
            let other = HostScrollWindow(contentRect: NSRect(x: 240, y: 220, width: 520, height: 340),
                                         styleMask: [.titled], backing: .buffered, defer: false)
            other.title = "Owned host scroll occluder"
            other.isReleasedWhenClosed = false
            other.collectionBehavior = window.collectionBehavior
            other.makeKeyAndOrderFront(nil)
            occluder = other
        case "restore":
            occluder?.orderOut(nil)
            occluder = nil
            focusWindow()
        default: break
        }
        var result = stats(kind: command)
        result["ack"] = true
        emitHostScrollFixture(result)
    }
}

@main
struct HostScrollFixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared
        let delegate = HostScrollFixture()
        app.setActivationPolicy(.regular)
        app.delegate = delegate
        app.finishLaunching()
        delegate.show()
        withExtendedLifetime(delegate) { app.run() }
    }
}
