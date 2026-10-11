import AppKit
import os

@MainActor
private func emitKeyFixture(_ value: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: value) else { return }
    data.append(10); FileHandle.standardOutput.write(data)
}

@MainActor
final class KeyProofWindow: NSWindow {
    override func accessibilityTitle() -> String? { title }
}

@MainActor
final class KeyProofGroup: NSView {
    var ordered: [NSView] = []
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { "Key fixture group" }
    // AppKit owns child exposure, including cell and field-editor identities.
    // Returning the NSViews here can expose wrappers while hiding the actual
    // focused cell from the AXChildren tree.
}

@MainActor
final class KeyProofToggle: NSButton {
    nonisolated let axPressCalls = OSAllocatedUnfairLock(initialState: 0)
    var activations = 0
    override var acceptsFirstResponder: Bool { isEnabled }
    override func accessibilityPerformPress() -> Bool {
        axPressCalls.withLock { $0 += 1 }; return super.accessibilityPerformPress()
    }
    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self); super.mouseDown(with: event)
    }
    @objc func activated() { activations += 1 }
}

@MainActor
final class KeyProofNavigation: NSButton {
    var activations = 0
    var onActivate: (() -> Void)?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .radioButton }
    override func accessibilityTitle() -> String? { title }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    nonisolated override func accessibilityPerformPress() -> Bool {
        // Match the other owned fixtures: the AX request acknowledges the
        // action, while fresh Source evidence verifies its later UI effect.
        DispatchQueue.main.async { self.activated() }
        return true
    }
    @objc func activated() { activations += 1; onActivate?() }
}

/// Setup commands never invoke toggle/navigation actions or inject key events.
/// All keyboard readback comes from AppKit's real local event delivery.
@MainActor
final class KeyFixture: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    nonisolated let commands = OSAllocatedUnfairLock(initialState: [String]())
    let window = KeyProofWindow(contentRect: NSRect(x: 240, y: 220, width: 520, height: 340),
        styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let group = KeyProofGroup(frame: NSRect(x: 0, y: 0, width: 520, height: 340))
    var toggle = KeyProofToggle(title: "Key fixture toggle", target: nil, action: nil)
    var retiredToggles: [KeyProofToggle] = []
    let text = NSTextField(frame: NSRect(x: 30, y: 142, width: 460, height: 30))
    let secure = NSSecureTextField(frame: NSRect(x: 30, y: 96, width: 460, height: 30))
    let stateLabel = NSTextField(labelWithString: "Toggle: off")
    let textLabel = NSTextField(labelWithString: "Text: before")
    let pageLabel = NSTextField(labelWithString: "Page: Document")
    let document = KeyProofNavigation(title: "Document", target: nil, action: nil)
    let other = KeyProofNavigation(title: "Other", target: nil, action: nil)
    var page = "Document"
    var secureMode = false
    var generation = 1, compositionSetupCalls = 0
    var rawEvents: [[String: Any]] = []
    var rawEventCount = 0
    var monitor: Any?
    var timer: Timer?
    var occluder: NSWindow?

    func show() {
        window.title = "Computer Use Key Fixture — Document"; window.isReleasedWhenClosed = false
        // Keep the semantic AX window title, but omit AppKit's visual title
        // child, which does not support AXUIElementDestroyed notifications.
        // This fixture's tasks address the content controls, not title chrome.
        window.titleVisibility = .hidden
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .canJoinAllApplications]
        toggle.frame = NSRect(x: 30, y: 216, width: 360, height: 36)
        configureToggle(toggle)
        stateLabel.frame = NSRect(x: 30, y: 182, width: 460, height: 28)
        textLabel.frame = NSRect(x: 30, y: 54, width: 460, height: 28)
        text.stringValue = "before"; text.isEditable = true; text.delegate = self
        text.setAccessibilityLabel("MVP document")
        text.isAutomaticTextCompletionEnabled = false
        secure.isEditable = true; secure.isHidden = true; secure.setAccessibilityLabel("Key fixture secure field")
        // This is the real AppKit traversal loop. Tab still has to arrive as
        // native keyboard input; setup never invokes traversal itself.
        toggle.nextKeyView = text; text.nextKeyView = toggle; secure.nextKeyView = toggle
        for (navigation, x) in [(document, 30.0), (other, 190.0)] {
            navigation.frame = NSRect(x: x, y: 300, width: 140, height: 28)
            navigation.setButtonType(.radio); navigation.bezelStyle = .rounded
            navigation.target = navigation; navigation.action = #selector(KeyProofNavigation.activated)
        }
        document.state = .on; other.state = .off
        document.onActivate = { [weak self] in self?.setPage("Document") }
        other.onActivate = { [weak self] in self?.setPage("Other") }
        pageLabel.frame = NSRect(x: 352, y: 300, width: 150, height: 26)
        group.ordered = [document, other, pageLabel, toggle, stateLabel, text, secure, textLabel]
        for view in group.ordered { group.addSubview(view) }
        window.contentView?.addSubview(group)
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp, .flagsChanged]) { [weak self] event in
            MainActor.assumeIsolated { self?.record(event) }; return event
        }
        let controlTimer = Timer(timeInterval: 0.01, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.drainCommands() }
        }
        timer = controlTimer; RunLoop.main.add(controlTimer, forMode: .common); RunLoop.main.add(controlTimer, forMode: .eventTracking)
        focusWindow(); window.makeFirstResponder(toggle)
        let frame = screenFrame(toggle)
        emitKeyFixture(["kind": "ready", "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": "com.lingxi.computer-user.guard-fixture", "window_id": window.windowNumber,
            "role": "AXCheckBox", "label": "Key fixture toggle", "frame": frame,
            "x": (frame["x"] ?? 0) + (frame["width"] ?? 0) / 2,
            "y": (frame["y"] ?? 0) + (frame["height"] ?? 0) / 2,
            "text_label": "MVP document", "text_frame": screenFrame(text), "secure_frame": screenFrame(secure),
            "composition_setup_is_fixture_truth_only": true,
            "controls": ["stats", "focus-window", "focus-toggle", "focus-text", "focus-secure", "focus-none", "occlude", "restore", "replace-control", "disable-toggle", "enable-toggle", "mark-text", "unmark-text", "quit"]])
        Thread { [commands] in
            let allowed = ["stats", "focus-window", "focus-toggle", "focus-text", "focus-secure", "focus-none", "occlude", "restore", "replace-control", "disable-toggle", "enable-toggle", "mark-text", "unmark-text", "quit"]
            while let line = readLine(), line.utf8.count < 64, allowed.contains(line) {
                let queued = commands.withLock { pending in
                    guard pending.count < 32 else { return false }; pending.append(line); return true
                }
                guard queued else { return }
            }
            commands.withLock { $0.append("quit") }
        }.start()
    }
    func configureToggle(_ control: KeyProofToggle) {
        control.setButtonType(.switch); control.bezelStyle = .rounded
        control.target = self; control.action = #selector(toggleActivated(_:))
    }
    @objc func toggleActivated(_ sender: KeyProofToggle) {
        guard sender === toggle else { return }
        sender.activated(); stateLabel.stringValue = "Toggle: \(sender.state == .on ? "on" : "off")"
        emitKeyFixture(["kind": "toggle_activation", "value": sender.state == .on, "activation_calls": sender.activations])
    }
    func controlTextDidChange(_ notification: Notification) {
        guard notification.object as? NSTextField === text else { return }
        let current = (text.currentEditor() as? NSTextView)?.string ?? text.stringValue
        textLabel.stringValue = "Text: \(current)"
    }
    func record(_ event: NSEvent) {
        let isKey = event.type == .keyDown || event.type == .keyUp
        let secureFocused = secure.currentEditor() != nil || window.firstResponder === secure
        var record: [String: Any] = ["type": event.type.rawValue, "key_code": event.keyCode,
            "marker": event.cgEvent.map { String($0.getIntegerValueField(.eventSourceUserData)) } as Any? ?? NSNull(),
            "event_number": NSNull(), "event_number_available": false,
            "received_monotonic_ns": String(DispatchTime.now().uptimeNanoseconds),
            "event_source_unix_process_id": event.cgEvent.map { $0.getIntegerValueField(.eventSourceUnixProcessID) } as Any? ?? NSNull(),
            "event_source_user_data": event.cgEvent.map { String($0.getIntegerValueField(.eventSourceUserData)) } as Any? ?? NSNull(),
            "timestamp": event.timestamp, "flags": event.cgEvent.map { $0.flags.rawValue } as Any? ?? NSNull(),
            "cg_event_available": event.cgEvent != nil,
            "cg_type": event.cgEvent.map { $0.type.rawValue } as Any? ?? NSNull(),
            "cg_timestamp": event.cgEvent.map { String($0.timestamp) } as Any? ?? NSNull(),
            "autorepeat": isKey ? event.isARepeat as Any : NSNull(), "window_id": event.windowNumber,
            "unicode_withheld": secureFocused]
        if isKey && !secureFocused {
            record["characters_codepoints"] = (event.characters ?? "").unicodeScalars.map(\.value)
            if let cg = event.cgEvent {
                var required = 0
                cg.keyboardGetUnicodeString(maxStringLength: 0, actualStringLength: &required, unicodeString: nil)
                var units = [UniChar](repeating: 0, count: min(required, 256)), count = 0
                cg.keyboardGetUnicodeString(maxStringLength: units.count, actualStringLength: &count, unicodeString: &units)
                record["cg_unicode_units"] = Array(units.prefix(min(count, units.count)))
                record["cg_unicode_truncated"] = required > units.count
            }
        }
        rawEventCount += 1
        if rawEvents.count < 256 { rawEvents.append(record) }
        // Do not discard ambient/marker-zero events or old input. A count
        // beyond the trace budget becomes explicit, never a false small count.
        var notice = record; notice["kind"] = "raw_key_event"; emitKeyFixture(notice)
    }
    func focusWindow() { window.makeKeyAndOrderFront(nil); NSApplication.shared.activate(ignoringOtherApps: true) }
    func focusText() {
        secureMode = false; secure.isHidden = true
        window.makeFirstResponder(text)
        if let editor = text.currentEditor() as? NSTextView {
            editor.isAutomaticTextCompletionEnabled = false; editor.isAutomaticSpellingCorrectionEnabled = false
            if #available(macOS 15, *) { editor.writingToolsBehavior = .none }
        }
    }
    func setPage(_ newPage: String) {
        page = newPage; window.title = "Computer Use Key Fixture — \(page)"; pageLabel.stringValue = "Page: \(page)"
        document.state = page == "Document" ? .on : .off; other.state = page == "Other" ? .on : .off
        for view in [toggle, stateLabel, text, textLabel] { view.isHidden = page != "Document" }
        secure.isHidden = page != "Document" || !secureMode
        window.makeFirstResponder(nil); stats("navigation")
    }
    func replaceControl() {
        let old = toggle, replacement = KeyProofToggle(title: old.title, target: nil, action: nil)
        replacement.frame = old.frame; replacement.state = old.state; replacement.isEnabled = old.isEnabled; replacement.isHidden = old.isHidden
        configureToggle(replacement)
        replacement.nextKeyView = text; text.nextKeyView = replacement; secure.nextKeyView = replacement
        let wasFocused = window.firstResponder === old
        old.removeFromSuperview(); retiredToggles.append(old)
        group.addSubview(replacement)
        group.ordered = group.ordered.map { $0 === old ? replacement : $0 }
        toggle = replacement; generation += 1
        if wasFocused { window.makeFirstResponder(replacement) }
    }
    func drainCommands() {
        let pending = commands.withLock { pending in let result = pending; pending.removeAll(); return result }
        for command in pending {
            switch command {
            case "focus-window": focusWindow()
            case "focus-toggle": secureMode = false; secure.isHidden = true; window.makeFirstResponder(toggle)
            case "focus-text": focusText()
            case "focus-secure": secureMode = true; secure.isHidden = page != "Document"; window.makeFirstResponder(secure)
            case "focus-none": window.makeFirstResponder(nil)
            case "disable-toggle": toggle.isEnabled = false
            case "enable-toggle": toggle.isEnabled = true
            case "replace-control": replaceControl()
            case "mark-text":
                focusText()
                if let editor = text.currentEditor() as? NSTextView {
                    editor.setMarkedText("fixture-marked", selectedRange: NSRange(location: 0, length: 0), replacementRange: NSRange(location: NSNotFound, length: 0))
                    compositionSetupCalls += 1
                }
            case "unmark-text": (text.currentEditor() as? NSTextView)?.unmarkText()
            case "occlude":
                let otherWindow = KeyProofWindow(contentRect: window.contentRect(forFrameRect: window.frame), styleMask: [.titled], backing: .buffered, defer: false)
                otherWindow.title = "Owned key occluder"; otherWindow.isReleasedWhenClosed = false
                otherWindow.collectionBehavior = window.collectionBehavior; otherWindow.makeKeyAndOrderFront(nil); occluder = otherWindow
            case "restore": occluder?.orderOut(nil); occluder = nil; focusWindow()
            case "quit": NSApplication.shared.terminate(nil)
            default: break
            }
            stats(command)
        }
    }
    func screenFrame(_ view: NSView) -> [String: CGFloat] {
        let frame = view.window?.convertToScreen(view.convert(view.bounds, to: nil)) ?? .zero
        return ["x": frame.minX, "y": CGDisplayBounds(CGMainDisplayID()).height - frame.maxY, "width": frame.width, "height": frame.height]
    }
    func stats(_ kind: String) {
        let editor = text.currentEditor() as? NSTextView
        let secureFocused = secure.currentEditor() != nil || window.firstResponder === secure
        let marked = editor?.markedRange()
        emitKeyFixture(["kind": kind, "pid": ProcessInfo.processInfo.processIdentifier, "window_id": window.windowNumber,
            "page": page, "page_label": pageLabel.stringValue,
            "navigation_calls": document.activations + other.activations,
            "value": toggle.state == .on, "toggle_label": stateLabel.stringValue,
            "toggle_activation_calls": ([toggle] + retiredToggles).reduce(0) { $0 + $1.activations },
            "toggle_ax_press_calls": ([toggle] + retiredToggles).reduce(0) { $0 + $1.axPressCalls.withLock { $0 } },
            "toggle_focused": window.firstResponder === toggle, "toggle_enabled": toggle.isEnabled,
            "control_generation": generation, "toggle_frame": screenFrame(toggle),
            "text_value": editor?.string ?? text.stringValue, "text_label": textLabel.stringValue,
            "text_focused": text.currentEditor() != nil || window.firstResponder === text,
            "secure_focused": secureFocused, "secure_visible": !secure.isHidden, "secure_value_withheld": true,
            "first_responder_class": window.firstResponder.map { NSStringFromClass(type(of: $0)) } as Any? ?? NSNull(),
            "editor_has_marked_text": editor?.hasMarkedText() as Any? ?? NSNull(),
            "editor_marked_range": marked.map { ["location": $0.location, "length": $0.length] } as Any? ?? NSNull(),
            "editor_selected_range": editor.map { ["location": $0.selectedRange().location, "length": $0.selectedRange().length] } as Any? ?? NSNull(),
            "composition_setup_calls": compositionSetupCalls, "composition_is_fixture_truth_only": true,
            "raw_key_event_count": rawEventCount, "input_events": rawEvents, "input_events_truncated": rawEventCount > rawEvents.count,
            "window_key": window.isKeyWindow, "window_visible": window.isVisible, "app_active": NSApplication.shared.isActive])
    }
}

@main
struct KeyFixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared, fixture = KeyFixture()
        app.setActivationPolicy(.regular); app.delegate = fixture; app.finishLaunching(); fixture.show()
        withExtendedLifetime(fixture) { app.run() }
    }
}
