import AppKit
import os

@MainActor
final class ProofWindow: NSWindow {
    override func accessibilityTitle() -> String? { title }
}

@MainActor
final class ProofGroup: NSView {
    var name = "Fixture group"
    var ordered: [NSView] = []
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { name }
    override func accessibilityChildren() -> [Any]? { ordered.filter { !$0.isHidden } }
}

@MainActor
final class ProofButton: NSButton {
    var pointerDown = 0, pointerUp = 0
    var pointerEvents: [[String: Any]] = []
    private func recordPointer(_ event: NSEvent, down: Bool) {
        if down { pointerDown += 1 } else { pointerUp += 1; title = "Clicked by native pointer" }
        let record: [String: Any] = ["type": event.type.rawValue, "button": event.buttonNumber,
            "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
            "timestamp": event.timestamp,
            "flags": event.cgEvent?.flags.rawValue ?? 0, "down": down]
        pointerEvents.append(record)
        if pointerEvents.count > 16 { pointerEvents.removeFirst() }
        var notice = record; notice["kind"] = down ? "pointer_down" : "pointer_up"
        if let data = try? JSONSerialization.data(withJSONObject: notice) {
            FileHandle.standardOutput.write(data + Data([10]))
        }
    }
    override func mouseDown(with event: NSEvent) { recordPointer(event, down: true) }
    override func mouseUp(with event: NSEvent) { recordPointer(event, down: false) }
    override func rightMouseDown(with event: NSEvent) { recordPointer(event, down: true) }
    override func rightMouseUp(with event: NSEvent) { recordPointer(event, down: false) }
    override func otherMouseDown(with event: NSEvent) { recordPointer(event, down: true) }
    override func otherMouseUp(with event: NSEvent) { recordPointer(event, down: false) }
    nonisolated let pressCalls = OSAllocatedUnfairLock(initialState: 0)
    nonisolated let pressMode = OSAllocatedUnfairLock(initialState: "reject")
    nonisolated override func accessibilityPerformPress() -> Bool {
        pressCalls.withLock { $0 += 1 }
        let mode = pressMode.withLock { $0 }
        guard mode != "reject" else { return false }
        // A tiny pipe event lets the independent fault driver inject stop or
        // death after the real AX callback began, without feeding hidden truth
        // to the online source or execution request.
        let started = Data("{\"kind\":\"press_started\"}\n".utf8)
        started.withUnsafeBytes { _ = Darwin.write(STDOUT_FILENO, $0.baseAddress, $0.count) }
        DispatchQueue.main.async { self.title = "Pressed by native AX" }
        if mode == "delay" { usleep(200_000) }
        return true
    }
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .button }
    override func accessibilityTitle() -> String? { title }
    override func accessibilityChildren() -> [Any]? { [] }
    // The fixture exposes the same element through hitTest and children.
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
}

@MainActor
final class ProofTextField: NSTextField {
    nonisolated let requests = OSAllocatedUnfairLock(initialState: 0)
    nonisolated let mode = OSAllocatedUnfairLock(initialState: "accept")
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .textField }
    override func accessibilitySubrole() -> NSAccessibility.Subrole? { nil }
    override func accessibilityTitle() -> String? { "MVP document" }
    override func isAccessibilityFocused() -> Bool { currentEditor() != nil || window?.firstResponder === self }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    nonisolated override func setAccessibilityValue(_ accessibilityValue: Any?) {
        guard let value = accessibilityValue as? String else { return }
        requests.withLock { $0 += 1 }
        let delay = mode.withLock { $0 == "delay" }
        let notice = Data("{\"kind\":\"value_started\"}\n".utf8)
        notice.withUnsafeBytes { _ = Darwin.write(STDOUT_FILENO, $0.baseAddress, $0.count) }
        DispatchQueue.main.async { self.stringValue = value }
        if delay { usleep(200_000) }
    }
}

@MainActor
final class ProofToggle: NSButton {
    nonisolated let pointerOnly = CommandLine.arguments.contains("--toggle-pointer-only")
    nonisolated let valueState = OSAllocatedUnfairLock(initialState: false)
    nonisolated let valueCalls = OSAllocatedUnfairLock(initialState: 0)
    nonisolated let pressCalls = OSAllocatedUnfairLock(initialState: 0)
    var pointerDown = 0, pointerUp = 0
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .checkBox }
    override func accessibilityTitle() -> String? { "MVP toggle" }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    override func isAccessibilitySelectorAllowed(_ selector: Selector) -> Bool {
        if pointerOnly && ["setAccessibilityValue:", "accessibilityPerformPress"].contains(NSStringFromSelector(selector)) { return false }
        return super.isAccessibilitySelectorAllowed(selector)
    }
    nonisolated override func accessibilityValue() -> Any? { NSNumber(value: valueState.withLock { $0 ? 1 : 0 }) }
    nonisolated override func accessibilityPerformPress() -> Bool { pressCalls.withLock { $0 += 1 }; return true }
    nonisolated override func setAccessibilityValue(_ accessibilityValue: Any?) {
        guard let value = accessibilityValue as? NSNumber, [0, 1].contains(value.intValue) else { return }
        valueCalls.withLock { $0 += 1 }
        // Deliberate fixture no-effect success, not a device-side fault switch.
        // Only independent fresh AX evidence can justify the pointer fallback.
        let notice = Data("{\"kind\":\"toggle_value_started\"}\n".utf8)
        notice.withUnsafeBytes { _ = Darwin.write(STDOUT_FILENO, $0.baseAddress, $0.count) }
    }
    override func mouseDown(with event: NSEvent) { pointerDown += 1 }
    override func mouseUp(with event: NSEvent) {
        pointerUp += 1
        let on = valueState.withLock { $0.toggle(); return $0 }
        state = on ? .on : .off
    }
}

@MainActor
final class ProofNavigation: NSButton {
    nonisolated let calls = OSAllocatedUnfairLock(initialState: 0)
    var onPress: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .radioButton }
    override func accessibilityTitle() -> String? { title }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    nonisolated override func accessibilityPerformPress() -> Bool {
        calls.withLock { $0 += 1 }; DispatchQueue.main.async { self.onPress?() }; return true
    }
}

@MainActor
final class GuardFixture: NSObject, NSApplicationDelegate {
    var pointerInputEvents: [[String: Any]] = []
    var eventMonitor: Any?
    let textMode = CommandLine.arguments.contains("--text-mode")
    let toggleMode = CommandLine.arguments.contains("--toggle-mode")
    var window: NSWindow = ProofWindow(contentRect: NSRect(x: 240, y: 240, width: 400, height: 200),
        styleMask: [.titled, .closable], backing: .buffered, defer: false)
    var occluder: NSWindow?
    let group = ProofGroup(frame: NSRect(x: 0, y: 0, width: 400, height: 200))
    let button = ProofButton(title: "Guard fixture button", target: nil, action: nil)
    let sibling = ProofButton(title: "Guard fixture button", target: nil, action: nil)
    let textField = ProofTextField(frame: NSRect(x: 20, y: 130, width: 340, height: 28))
    let document = ProofNavigation(title: "Document", target: nil, action: nil)
    let other = ProofNavigation(title: "Other", target: nil, action: nil)
    let toggle = ProofToggle(title: "MVP toggle", target: nil, action: nil)
    func show() {
        eventMonitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp, .otherMouseDown, .otherMouseUp]) { [weak self] event in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.pointerInputEvents.append(["type": event.type.rawValue, "button": event.buttonNumber,
                    "timestamp": event.timestamp, "marker": String(event.cgEvent?.getIntegerValueField(.eventSourceUserData) ?? 0),
                    "flags": event.cgEvent?.flags.rawValue ?? 0,
                    "down": [.leftMouseDown, .rightMouseDown, .otherMouseDown].contains(event.type)])
                if self.pointerInputEvents.count > 16 { self.pointerInputEvents.removeFirst() }
            }
            return event
        }
        window.title = "Computer Use Guard Fixture"
        window.isReleasedWhenClosed = false
        // The owned fixture accompanies the owned Operator in application sets
        // as well as Spaces; switching to its consent UI must not hide it.
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .canJoinAllApplications]
        button.frame = NSRect(x: 70, y: 70, width: 240, height: 40)
        button.bezelStyle = .rounded
        sibling.frame = NSRect(x: 10, y: 10, width: 240, height: 40)
        sibling.bezelStyle = .rounded
        group.addSubview(button); group.addSubview(sibling); group.ordered = [button, sibling]
        if textMode {
            // Keep system title-bar sharing UI outside this owned text fixture;
            // ProofWindow still exposes the actual page title through AX.
            window.titleVisibility = .hidden
            textField.stringValue = "before"; textField.isEditable = true
            // This fixture measures registered setter/navigation calls, not
            // macOS completion UI. Keep the editor's auxiliary UI deterministic.
            textField.isAutomaticTextCompletionEnabled = false
            group.addSubview(textField); group.ordered.append(textField); textField.setAccessibilityParent(group)
            for (nav, x) in [(document, 20.0), (other, 190.0)] {
                nav.frame = NSRect(x: x, y: 170, width: 150, height: 25); nav.setButtonType(.radio)
                group.addSubview(nav); group.ordered.append(nav); nav.setAccessibilityParent(group)
            }
            window.title = "Computer Use Guard Fixture — Document"
            if toggleMode {
                toggle.setButtonType(.switch); toggle.frame = NSRect(x: 260, y: 10, width: 130, height: 25)
                group.addSubview(toggle); group.ordered.append(toggle); toggle.setAccessibilityParent(group)
            }
            document.onPress = { [weak self] in self?.window.title = "Computer Use Guard Fixture — Document"; self?.textField.isHidden = false; self?.toggle.isHidden = false; self?.focusText() }
            other.onPress = { [weak self] in self?.window.title = "Computer Use Guard Fixture — Other"; self?.textField.isHidden = true; self?.toggle.isHidden = true; self?.window.makeFirstResponder(nil) }
        }
        button.setAccessibilityParent(group); sibling.setAccessibilityParent(group); group.setAccessibilityParent(window)
        window.contentView?.addSubview(group)
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        if textMode { focusText() }
        let frame = button.accessibilityFrame()
        let height = CGDisplayBounds(CGMainDisplayID()).height
        report(["kind": "ready", "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": "com.lingxi.computer-user.guard-fixture", "window_id": window.windowNumber,
            "x": frame.midX, "y": height - frame.midY, "role": "AXButton", "label": button.title,
            "frame": ["x": frame.minX, "y": height - frame.maxY, "width": frame.width, "height": frame.height]])
        // Only the test pipe can show/hide this fixture's own occluding window.
        Thread { [self] in
            while let line = readLine(), line.utf8.count < 64 {
                guard ["occlude", "restore", "rename-group", "restore-group", "reorder", "restore-order", "swap-buttons", "replace-window", "inconsistent-parent", "restore-parent", "duplicate-child", "overfull-children", "stats", "accept-press", "delay-press", "reset-press", "accept-value", "delay-value", "focus-window", "focus-button", "focus-text"].contains(line) else { return }
                DispatchQueue.main.async { self.control(line) }
            }
        }.start()
    }
    func report(_ value: [String: Any]) {
        guard var data = try? JSONSerialization.data(withJSONObject: value) else { return }
        data.append(10); FileHandle.standardOutput.write(data)
    }
    func focusText() {
        window.makeFirstResponder(textField)
        if let editor = textField.currentEditor() as? NSTextView {
            editor.isAutomaticTextCompletionEnabled = false
            editor.isAutomaticSpellingCorrectionEnabled = false
            if #available(macOS 15, *) { editor.writingToolsBehavior = .none }
        }
    }
    func control(_ command: String) {
        if command == "focus-window" {
            window.makeKeyAndOrderFront(nil)
            NSApplication.shared.activate(ignoringOtherApps: true)
        } else if command == "accept-value" || command == "delay-value" {
            textField.mode.withLock { $0 = command == "delay-value" ? "delay" : "accept" }
            textField.stringValue = "before"; focusText()
        } else if command == "focus-button" { window.makeFirstResponder(button) }
        else if command == "focus-text" { focusText() }
        else if command == "accept-press" || command == "delay-press" || command == "reset-press" {
            for target in [button, sibling] {
                target.pressMode.withLock { $0 = command == "accept-press" ? "accept" : command == "delay-press" ? "delay" : "reject" }
                target.title = "Guard fixture button"
            }
        } else if command == "occlude" {
            let other = NSWindow(contentRect: window.frame, styleMask: [.titled], backing: .buffered, defer: false)
            other.title = "Owned occluder"
            other.isReleasedWhenClosed = false
            other.collectionBehavior = window.collectionBehavior
            other.makeKeyAndOrderFront(nil); occluder = other
        } else if command == "restore" {
            occluder?.orderOut(nil); occluder = nil; window.makeKeyAndOrderFront(nil)
        } else if command == "rename-group" || command == "restore-group" {
            group.name = command == "rename-group" ? "Another group" : "Fixture group"
        } else if command == "reorder" || command == "restore-order" {
            group.ordered = command == "reorder" ? [sibling, button] : [button, sibling]
        } else if command == "inconsistent-parent" || command == "restore-parent" {
            button.setAccessibilityParent(command == "inconsistent-parent" ? window : group)
        } else if command == "duplicate-child" || command == "overfull-children" {
            group.ordered = command == "duplicate-child" ? [button, button, sibling] : [button] + Array(repeating: sibling, count: 256)
        } else if command == "swap-buttons" {
            let previous = button.frame; button.frame = sibling.frame; sibling.frame = previous
        } else if command == "replace-window" {
            let other = ProofWindow(contentRect: NSRect(x: 240, y: 240, width: 400, height: 200),
                styleMask: [.titled, .closable], backing: .buffered, defer: false)
            other.title = window.title; other.isReleasedWhenClosed = false
            other.collectionBehavior = window.collectionBehavior
            group.removeFromSuperview(); other.contentView?.addSubview(group)
            window.orderOut(nil); window.close(); window = other; group.setAccessibilityParent(window); window.makeKeyAndOrderFront(nil)
        }
        report(["kind": command, "window_id": window.windowNumber, "press_calls": button.pressCalls.withLock { $0 } + sibling.pressCalls.withLock { $0 }, "visible_title": button.title, "value_calls": textField.requests.withLock { $0 }, "navigation_calls": document.calls.withLock { $0 } + other.calls.withLock { $0 },
            "window_visible": window.isVisible, "window_key": window.isKeyWindow,
            "window_miniaturized": window.isMiniaturized, "app_active": NSApplication.shared.isActive,
            "app_hidden": NSApplication.shared.isHidden, "app_window_ids": NSApplication.shared.windows.map(\.windowNumber),
            "app_ax_window_count": NSApplication.shared.accessibilityWindows()?.count ?? -1,
            "window_ax_element": window.isAccessibilityElement(), "window_ax_hidden": window.isAccessibilityHidden(),
            "app_windows": NSApplication.shared.windows.map { ["id": $0.windowNumber, "class": NSStringFromClass(type(of: $0)), "visible": $0.isVisible] as [String: Any] },
            "pointer_down_calls": button.pointerDown + sibling.pointerDown,
            "pointer_up_calls": button.pointerUp + sibling.pointerUp,
            "pointer_inside_button": button.frame.contains(window.mouseLocationOutsideOfEventStream),
            "pointer_input_events": pointerInputEvents,
            "pointer_events": button.pointerEvents + sibling.pointerEvents])
        if toggleMode {
            report(["kind": "toggle_stats", "value": toggle.valueState.withLock { $0 },
                "press_calls": toggle.pressCalls.withLock { $0 },
                "value_calls": toggle.valueCalls.withLock { $0 }, "pointer_down_calls": toggle.pointerDown, "pointer_up_calls": toggle.pointerUp])
        }
    }
}

@main
struct FixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared, delegate = GuardFixture()
        app.setActivationPolicy(.regular); app.delegate = delegate; app.finishLaunching()
        delegate.show()
        withExtendedLifetime(delegate) { app.run() }
    }
}
