import AppKit
import Darwin
import Foundation

/// A cooperating app's real AX action: schedule its own control change without
/// NSButton's default click animation or window-focus behavior. This fixture
/// proves this explicit capability, not arbitrary third-party AX callbacks.
@MainActor
final class BackgroundToggleButton: NSButton {
    nonisolated override func accessibilityPerformPress() -> Bool {
        DispatchQueue.main.async {
            guard self.isEnabled else { return }
            self.state = self.state == .on ? .off : .on
            _ = self.sendAction(self.action, to: self.target)
        }
        return true
    }
}

/// Owned diagnostic fixture. State files belong only to the external evaluator.
@MainActor
final class BackgroundFixtureDelegate: NSObject, NSApplicationDelegate {
    let snapshotPath: String
    private var window: NSWindow?
    private var toggle: NSButton?
    private var monitor: Any?
    private var actionCount = 0
    private var pointerCount = 0
    private var keyCount = 0

    init(snapshotPath: String) { self.snapshotPath = snapshotPath }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(
            contentRect: NSRect(x: 100, y: 180, width: 360, height: 180),
            styleMask: [.titled, .closable], backing: .buffered, defer: false
        )
        window.title = "Background AX Fixture"
        let toggle = BackgroundToggleButton(checkboxWithTitle: "Background toggle", target: self, action: #selector(changed(_:)))
        toggle.frame = NSRect(x: 24, y: 70, width: 250, height: 32)
        window.contentView?.addSubview(toggle)
        self.window = window
        self.toggle = toggle
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown, .keyDown]) { [weak self] event in
            guard let self else { return event }
            if event.type == .keyDown { self.keyCount += 1 } else { self.pointerCount += 1 }
            self.writeSnapshot()
            return event
        }
        window.orderBack(nil)
        writeSnapshot()
        let ready: [String: Any] = ["pid": getpid(), "window_number": window.windowNumber]
        if let data = try? JSONSerialization.data(withJSONObject: ready) {
            FileHandle.standardOutput.write(data + Data([10]))
        }
    }

    @objc private func changed(_ sender: NSButton) {
        actionCount += 1
        // Evaluator file I/O must not delay the actual AX action callback.
        DispatchQueue.main.async { [weak self] in self?.writeSnapshot() }
    }

    private func writeSnapshot() {
        let state: [String: Any] = [
            "value": toggle?.state == .on,
            "action_count": actionCount,
            "pointer_event_count": pointerCount,
            "key_event_count": keyCount,
        ]
        do {
            let data = try JSONSerialization.data(withJSONObject: state, options: [.sortedKeys])
            try data.write(to: URL(fileURLWithPath: snapshotPath), options: [.atomic])
            try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: snapshotPath)
        } catch {
            fputs("background-fixture: snapshot-unavailable\n", stderr)
            NSApplication.shared.terminate(nil)
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let monitor { NSEvent.removeMonitor(monitor) }
    }
}

@main
@MainActor
struct BackgroundAXFixtureMain {
    private static var retainedDelegate: BackgroundFixtureDelegate?
    static func main() {
        let args = CommandLine.arguments
        guard args.count == 3, args[1] == "--snapshot", args[2].hasPrefix("/"),
              !FileManager.default.fileExists(atPath: args[2]) else {
            fputs("background-fixture: startup-rejected\n", stderr)
            exit(2)
        }
        let application = NSApplication.shared
        application.setActivationPolicy(.regular)
        let delegate = BackgroundFixtureDelegate(snapshotPath: args[2])
        retainedDelegate = delegate
        application.delegate = delegate
        application.run()
    }
}
