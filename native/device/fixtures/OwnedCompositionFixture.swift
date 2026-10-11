// Owned standard NSTextView fixture for readonly public AX marked-range comparison.
import AppKit
import Foundation
import os

private let compositionFixtureBundleID = "com.lingxi.computer-user.guard-fixture"

@MainActor
private func emitCompositionFixture(_ record: [String: Any]) {
    guard var data = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]) else { return }
    data.append(10)
    FileHandle.standardOutput.write(data)
}

@MainActor
private final class OwnedCompositionTextView: NSTextView {
    var keyEventCount = 0

    override func keyDown(with event: NSEvent) {
        keyEventCount += 1
        super.keyDown(with: event)
    }

    override func keyUp(with event: NSEvent) {
        keyEventCount += 1
        super.keyUp(with: event)
    }
}

@MainActor
private final class OwnedCompositionFixture: NSObject, NSApplicationDelegate {
    nonisolated let commands = OSAllocatedUnfairLock(initialState: [String]())
    let window = NSWindow(contentRect: NSRect(x: 240, y: 220, width: 640, height: 420),
                          styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
    let scrollView = NSScrollView(frame: NSRect(x: 20, y: 20, width: 600, height: 360))
    let textView = OwnedCompositionTextView(frame: NSRect(x: 0, y: 0, width: 600, height: 360))
    var timer: Timer?

    func show() {
        window.title = "Owned Composition Fixture"
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.borderType = .bezelBorder
        scrollView.documentView = textView
        textView.string = "fixture baseline"
        textView.isEditable = true
        textView.isSelectable = true
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.isAutomaticTextCompletionEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.textContainer?.widthTracksTextView = true
        window.contentView?.addSubview(scrollView)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        window.makeFirstResponder(textView)
        textView.setSelectedRange(NSRange(location: textView.string.utf16.count, length: 0))

        let controlTimer = Timer(timeInterval: 0.01, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.drainCommands() }
        }
        timer = controlTimer
        RunLoop.main.add(controlTimer, forMode: .common)
        RunLoop.main.add(controlTimer, forMode: .eventTracking)
        emitCompositionFixture([
            "kind": "ready",
            "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": compositionFixtureBundleID,
            "window_id": window.windowNumber,
            "window_title": window.title,
            "fixture": "standard_NSTextView_inside_NSScrollView",
            "fixture_marked_state_is_truth_only": true,
            "key_event_count": textView.keyEventCount,
            "controls": ["stats", "focus-text", "mark-text", "unmark-text", "quit"]
        ])

        Thread { [commands] in
            let allowed = ["stats", "focus-text", "mark-text", "unmark-text", "quit"]
            while let line = readLine(), line.utf8.count <= 64, allowed.contains(line) {
                let queued = commands.withLock { pending in
                    guard pending.count < 16 else { return false }
                    pending.append(line)
                    return true
                }
                guard queued else { return }
            }
            commands.withLock { $0.append("quit") }
        }.start()
    }

    private func focusText() {
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(textView)
    }

    private func drainCommands() {
        let pending = commands.withLock { items in
            let result = items
            items.removeAll()
            return result
        }
        for command in pending {
            switch command {
            case "focus-text": focusText()
            case "mark-text":
                focusText()
                let insertion = textView.selectedRange().location
                textView.setMarkedText("かな", selectedRange: NSRange(location: 0, length: 0),
                    replacementRange: NSRange(location: insertion, length: 0))
            case "unmark-text":
                focusText()
                textView.unmarkText()
            case "quit": NSApplication.shared.terminate(nil)
            default: break
            }
            stats(command)
        }
    }

    private func stats(_ kind: String) {
        let range = textView.markedRange()
        emitCompositionFixture([
            "kind": kind,
            "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": compositionFixtureBundleID,
            "window_id": window.windowNumber,
            "first_responder_is_text_view": window.firstResponder === textView,
            "has_marked_text": textView.hasMarkedText(),
            "marked_range": ["location": String(range.location), "length": String(range.length)],
            "key_event_count": textView.keyEventCount,
            "fixture_marked_state_is_truth_only": true,
            "input_posted": false,
            "ax_set_value_calls": 0,
            "ax_press_calls": 0,
            "set_focus_calls": 0,
            "cg_post_event_calls": 0
        ])
    }
}

@main
struct OwnedCompositionFixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared
        let fixture = OwnedCompositionFixture()
        app.setActivationPolicy(.regular)
        app.delegate = fixture
        app.finishLaunching()
        fixture.show()
        withExtendedLifetime(fixture) { app.run() }
    }
}
