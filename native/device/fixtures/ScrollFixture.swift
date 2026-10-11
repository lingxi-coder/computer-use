import AppKit

@MainActor
private func emitScrollFixture(_ value: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: value) else { return }
    data.append(10); FileHandle.standardOutput.write(data)
}

@MainActor
final class ScrollProofWindow: NSWindow {
    override func accessibilityTitle() -> String? { title }
}

@MainActor
final class ScrollProofGroup: NSView {
    var ordered: [NSView] = []
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { "Scroll fixture group" }
    override func accessibilityChildren() -> [Any]? { ordered.filter { !$0.isHidden } }
}

@MainActor
final class ScrollDocument: NSView {
    override var isFlipped: Bool { true }
    override func isAccessibilityElement() -> Bool { false }
    override func draw(_ dirtyRect: NSRect) {
        NSColor.textBackgroundColor.setFill(); dirtyRect.fill()
        let attributes: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedSystemFont(ofSize: 15, weight: .regular),
                                                        .foregroundColor: NSColor.labelColor]
        for index in 0..<48 {
            let y = CGFloat(index * 32 + 12)
            if dirtyRect.intersects(NSRect(x: 12, y: y, width: 1100, height: 26)) {
                ("Row \(index + 1) — bounded native scroll fixture" as NSString).draw(at: NSPoint(x: 12, y: y), withAttributes: attributes)
            }
        }
    }
}

@MainActor
final class ScrollProofView: NSScrollView {
    var name = "MVP scroll document"
    var scrollCalls = 0
    var events: [[String: Any]] = []
    var afterScroll: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .scrollArea }
    override func accessibilityTitle() -> String? { name }
    override func isAccessibilityEnabled() -> Bool { true }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    override func reflectScrolledClipView(_ clipView: NSClipView) {
        super.reflectScrolledClipView(clipView)
        // AppKit may animate the real clip offset after scrollWheel returns.
        // Update visible evidence for each actual bounds change, including the
        // final animation offset, rather than sampling only the event handler.
        if clipView === contentView { afterScroll?() }
    }
    override func scrollWheel(with event: NSEvent) {
        scrollCalls += 1
        let record: [String: Any] = ["type": event.type.rawValue,
            "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
            "timestamp": event.timestamp, "flags": event.cgEvent?.flags.rawValue ?? 0,
            "dx": event.scrollingDeltaX, "dy": event.scrollingDeltaY,
            "precise": event.hasPreciseScrollingDeltas, "phase": event.phase.rawValue,
            "momentum_phase": event.momentumPhase.rawValue,
            "raw_dx": event.cgEvent?.getIntegerValueField(.scrollWheelEventPointDeltaAxis2) ?? 0,
            "raw_dy": event.cgEvent?.getIntegerValueField(.scrollWheelEventPointDeltaAxis1) ?? 0]
        events.append(record); if events.count > 16 { events.removeFirst() }
        // Actual NSScrollView behavior supplies the effect; counters and the
        // test pipe never update offsets or tell Source that scrolling worked.
        super.scrollWheel(with: event)
        afterScroll?()
        var notice = record; notice["kind"] = "scroll_event"
        notice["offset_x"] = contentView.bounds.origin.x; notice["offset_y"] = contentView.bounds.origin.y
        emitScrollFixture(notice)
    }
}

@MainActor
final class ScrollPreparationButton: NSButton {
    var downCalls = 0, upCalls = 0
    var onRelease: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .button }
    override func accessibilityTitle() -> String? { title }
    override func accessibilityChildren() -> [Any]? { [] }
    override func accessibilityFrame() -> NSRect {
        // NSButton's bezel accessibility frame can be inset and asymmetric.
        // The preparation target occupies the full scroll-view bounds, so its
        // independently observed AX center must use that same screen geometry.
        guard let window else { return .zero }
        return window.convertToScreen(convert(bounds, to: nil))
    }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    override func mouseDown(with event: NSEvent) { downCalls += 1 }
    override func mouseUp(with event: NSEvent) {
        upCalls += 1; onRelease?()
        emitScrollFixture(["kind": "preparation_pointer_up", "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
                           "timestamp": event.timestamp, "down_calls": downCalls, "up_calls": upCalls])
    }
}

/// Package with the same com.lingxi.computer-user.guard-fixture bundle ID.
/// Initially only the preparation button occupies the scroll frame. The runner
/// obtains its proof from Source and sends an independently authorized atomic
/// Click. That release exposes the real NSScrollView at the existing cursor,
/// after which the runner obtains a fresh AXScrollArea proof for Scroll.
@MainActor
final class ScrollFixture: NSObject, NSApplicationDelegate {
    let window = ScrollProofWindow(contentRect: NSRect(x: 240, y: 220, width: 520, height: 360),
                                  styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let group = ScrollProofGroup(frame: NSRect(x: 0, y: 0, width: 520, height: 360))
    let scroll = ScrollProofView(frame: NSRect(x: 20, y: 60, width: 480, height: 280))
    let preparation = ScrollPreparationButton(title: "Prepare bounded scroll", target: nil, action: nil)
    let offsetLabel = NSTextField(labelWithString: "Scroll offset x=0 y=0")
    var occluder: NSWindow?
    var eventMonitor: Any?
    var inputEvents: [[String: Any]] = []
    var rawScrollCalls = 0

    func show() {
        window.title = "Computer Use Scroll Fixture"; window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .canJoinAllApplications]
        scroll.hasVerticalScroller = true; scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = false; scroll.borderType = .bezelBorder
        scroll.documentView = ScrollDocument(frame: NSRect(x: 0, y: 0, width: 1200, height: 1600))
        scroll.isHidden = true
        offsetLabel.frame = NSRect(x: 20, y: 18, width: 480, height: 28)
        preparation.frame = scroll.frame; preparation.bezelStyle = .rounded
        for view in [scroll, preparation, offsetLabel] {
            group.addSubview(view); view.setAccessibilityParent(group)
        }
        group.ordered = [preparation, scroll, offsetLabel]; group.setAccessibilityParent(window)
        window.contentView?.addSubview(group)
        preparation.onRelease = { [weak self] in self?.prepare() }
        scroll.afterScroll = { [weak self] in self?.updateOffsetLabel() }
        eventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .leftMouseDown, .leftMouseUp,
            .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp, .keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            MainActor.assumeIsolated {
                guard let self else { return }
                if event.type == .scrollWheel { self.rawScrollCalls += 1 }
                self.inputEvents.append(["type": event.type.rawValue, "timestamp": event.timestamp,
                    "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
                    "flags": event.cgEvent?.flags.rawValue ?? 0])
                if self.inputEvents.count > 32 { self.inputEvents.removeFirst() }
            }
            return event
        }
        focusWindow()
        emitScrollFixture(targetRecord(kind: "ready", view: preparation, role: "AXButton", label: preparation.title))
        Thread { [self] in
            while let command = readLine(), command.utf8.count < 64 {
                guard ["stats", "reset", "focus-window", "rename-scroll", "restore-label", "occlude", "restore", "quit"].contains(command) else { break }
                DispatchQueue.main.async { self.control(command) }
                if command == "quit" { return }
            }
            DispatchQueue.main.async { NSApplication.shared.terminate(nil) }
        }.start()
    }

    func focusWindow() {
        window.makeKeyAndOrderFront(nil); NSApplication.shared.activate(ignoringOtherApps: true)
    }
    func updateOffsetLabel() {
        let point = scroll.contentView.bounds.origin
        offsetLabel.stringValue = "Scroll offset x=\(Int(point.x.rounded())) y=\(Int(point.y.rounded()))"
    }
    func prepare() {
        preparation.isHidden = true; scroll.isHidden = false
        updateOffsetLabel()
        emitScrollFixture(targetRecord(kind: "prepared", view: scroll, role: "AXScrollArea", label: scroll.name))
    }
    func targetRecord(kind: String, view: NSView, role: String, label: String) -> [String: Any] {
        let frame = view.accessibilityFrame(), height = CGDisplayBounds(CGMainDisplayID()).height
        return ["kind": kind, "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": "com.lingxi.computer-user.guard-fixture", "window_id": window.windowNumber,
            "role": role, "label": label, "x": frame.midX, "y": height - frame.midY,
            "frame": ["x": frame.minX, "y": height - frame.maxY, "width": frame.width, "height": frame.height]]
    }
    func control(_ command: String) {
        switch command {
        case "quit": NSApplication.shared.terminate(nil); return
        case "focus-window": focusWindow()
        case "reset":
            // Explicit test setup only. It is neither a scroll delivery nor
            // input authority, and the runner must acquire fresh Source proof.
            scroll.contentView.scroll(to: NSPoint(x: 160, y: 160)); scroll.reflectScrolledClipView(scroll.contentView)
            preparation.isHidden = false; scroll.isHidden = true
            scroll.scrollCalls = 0; scroll.events = []; rawScrollCalls = 0; inputEvents = []
            preparation.downCalls = 0; preparation.upCalls = 0; updateOffsetLabel()
        case "rename-scroll": scroll.name = "Changed scroll document"
        case "restore-label": scroll.name = "MVP scroll document"
        case "occlude":
            let other = ScrollProofWindow(contentRect: NSRect(x: 240, y: 220, width: 520, height: 360),
                                          styleMask: [.titled], backing: .buffered, defer: false)
            other.title = "Owned scroll occluder"; other.isReleasedWhenClosed = false
            other.collectionBehavior = window.collectionBehavior; other.makeKeyAndOrderFront(nil); occluder = other
        case "restore": occluder?.orderOut(nil); occluder = nil; focusWindow()
        default: break
        }
        let point = scroll.contentView.bounds.origin
        emitScrollFixture(["kind": command, "window_id": window.windowNumber,
            "raw_scroll_calls": rawScrollCalls, "scroll_calls": scroll.scrollCalls,
            "pointer_down_calls": preparation.downCalls, "pointer_up_calls": preparation.upCalls,
            "input_events": inputEvents, "scroll_events": scroll.events,
            "offset_x": point.x, "offset_y": point.y, "visible_offset": offsetLabel.stringValue,
            "scrollbar_values": ["horizontal": scroll.horizontalScroller?.doubleValue as Any? ?? NSNull(),
                                 "vertical": scroll.verticalScroller?.doubleValue as Any? ?? NSNull()],
            "prepared": preparation.isHidden, "window_key": window.isKeyWindow, "app_active": NSApplication.shared.isActive])
    }
}

@main
struct ScrollFixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared, delegate = ScrollFixture()
        app.setActivationPolicy(.regular); app.delegate = delegate; app.finishLaunching()
        delegate.show(); withExtendedLifetime(delegate) { app.run() }
    }
}
