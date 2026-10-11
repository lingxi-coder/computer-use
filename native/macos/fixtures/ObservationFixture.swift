import AppKit

/// An owned, disposable observation surface. It reads no user files, performs no
/// input injection, has no editable/secure fields, and makes no network calls.
@MainActor
final class ObservationFixtureDelegate: NSObject, NSApplicationDelegate {
    private var window: NSWindow?
    private var diagnosticTimer: Timer?

    private struct FixtureStatus: Encodable {
        let pid: Int32
        let uptime_ns: UInt64
        let window_visible: Bool
        let window_miniaturized: Bool
        let window_on_active_space: Bool
        let window_occlusion_visible: Bool
        let window_level: Int
        let collection_behavior: UInt
        let application_active: Bool
        let application_hidden: Bool
    }

    func startDiagnosticsIfRequested() {
        guard ProcessInfo.processInfo.environment["CU_M2_FIXTURE_DIAGNOSTICS"] == "1" else { return }
        // A selector timer scheduled exclusively on the main run loop keeps
        // AppKit state on its owning thread; no Sendable closure crosses actors.
        let timer = Timer(timeInterval: 0.5, target: self, selector: #selector(reportFixtureStatus(_:)), userInfo: nil, repeats: true)
        diagnosticTimer = timer
        RunLoop.main.add(timer, forMode: .common)
        reportFixtureStatus(timer)
    }

    @objc private func reportFixtureStatus(_ timer: Timer) {
        guard let window else { return }
        let status = FixtureStatus(pid: ProcessInfo.processInfo.processIdentifier,
            uptime_ns: DispatchTime.now().uptimeNanoseconds,
            window_visible: window.isVisible, window_miniaturized: window.isMiniaturized,
            window_on_active_space: window.isOnActiveSpace,
            window_occlusion_visible: window.occlusionState.contains(.visible),
            window_level: window.level.rawValue, collection_behavior: window.collectionBehavior.rawValue,
            application_active: NSApplication.shared.isActive, application_hidden: NSApplication.shared.isHidden)
        guard let data = try? JSONEncoder().encode(status) else { return }
        FileHandle.standardOutput.write(Data("FIXTURE_STATUS ".utf8) + data + Data("\n".utf8))
    }

    func showFixtureWindow() {
        let window = NSWindow(contentRect: NSRect(x: 160, y: 160, width: 640, height: 300),
                              styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.title = "Computer Use M2 Fixture"
        window.isReleasedWhenClosed = false
        window.level = .normal
        // Keep the owned fixture at the normal level and allow it alongside
        // full-screen apps, without switching the user's current space.
        // Visibility still depends on the macOS session being unlocked.
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        let content = NSView(frame: NSRect(x: 0, y: 0, width: 640, height: 300))
        content.wantsLayer = true
        content.layer?.backgroundColor = NSColor.white.cgColor

        let label = NSTextField(labelWithString: "Computer Use M2 Fixture")
        label.font = NSFont.systemFont(ofSize: 28, weight: .semibold)
        label.textColor = .black
        label.frame = NSRect(x: 32, y: 205, width: 576, height: 45)
        label.setAccessibilityIdentifier("m2-fixture-label")
        content.addSubview(label)

        let detail = NSTextField(labelWithString: "Owned observation surface — no user documents")
        detail.font = NSFont.systemFont(ofSize: 17)
        detail.textColor = .darkGray
        detail.frame = NSRect(x: 32, y: 164, width: 576, height: 28)
        content.addSubview(detail)

        let checkbox = NSButton(checkboxWithTitle: "Observation fixture checkbox", target: nil, action: nil)
        checkbox.frame = NSRect(x: 32, y: 111, width: 380, height: 32)
        checkbox.state = .off
        checkbox.setAccessibilityIdentifier("m2-fixture-checkbox")
        content.addSubview(checkbox)

        let button = NSButton(title: "Fixture button", target: nil, action: nil)
        button.frame = NSRect(x: 32, y: 52, width: 170, height: 34)
        button.bezelStyle = .rounded
        button.setAccessibilityIdentifier("m2-fixture-button")
        content.addSubview(button)
        window.contentView = content
        self.window = window
        window.makeKeyAndOrderFront(nil)
        // The test explicitly opens this owned fixture in front. macOS 14's
        // cooperative activate() can leave it hidden behind a full-screen app
        // that has not yielded activation. This affects only the fixture UI.
        NSApplication.shared.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
}

@main
struct ObservationFixture {
    @MainActor
    static func main() {
        let application = NSApplication.shared
        let delegate = ObservationFixtureDelegate()
        application.setActivationPolicy(.regular)
        application.delegate = delegate
        // This fixture is also launched directly by the smoke-test subprocess,
        // without Launch Services or a nib. Finish initialization and construct
        // its owned window explicitly before entering the event loop.
        application.finishLaunching()
        delegate.showFixtureWindow()
        FileHandle.standardOutput.write(Data("READY\n".utf8))
        delegate.startDiagnosticsIfRequested()
        // NSApplication.delegate is weak; this explicit lifetime keeps the
        // fixture's main-actor delegate alive for the entire event loop.
        withExtendedLifetime(delegate) { application.run() }
    }
}
