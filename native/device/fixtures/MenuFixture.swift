import AppKit
import os

@MainActor
final class MenuProofWindow: NSWindow {
    override func accessibilityTitle() -> String? { title }
}

@MainActor
final class MenuProofGroup: NSView {
    var trigger: NSView?
    var selectionLabel: NSView?
    var pageLabel: NSView?
    var navigation: [NSView] = []
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .group }
    override func accessibilityLabel() -> String? { "Menu fixture group" }
    override func accessibilityChildren() -> [Any]? {
        (navigation + [pageLabel, trigger, selectionLabel].compactMap { $0 }).filter { !$0.isHidden }
    }
}

@MainActor
final class MenuProofNavigation: NSButton {
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

/// This is an actual native menu trigger, not a callback that fabricates a
/// visible menu. NSMenu's own AX objects and WindowServer windows are sampled
/// separately in stats. The callback count remains independent of visibility.
@MainActor
final class MenuProofButton: NSButton {
    nonisolated let actionCalls = OSAllocatedUnfairLock(initialState: 0)
    nonisolated let mode = OSAllocatedUnfairLock(initialState: "reject")
    var openMenu: (() -> Void)?
    weak var selectionMenu: NSMenu?
    override func isAccessibilityElement() -> Bool { true }
    override func accessibilityRole() -> NSAccessibility.Role? { .menuButton }
    override func accessibilityTitle() -> String? { "Fixture native menu" }
    override func accessibilityValue() -> Any? {
        selectionMenu?.items.first(where: { $0.state == .on })?.title ?? "No selection"
    }
    override func accessibilityChildren() -> [Any]? { [] }
    nonisolated override func accessibilityHitTest(_ point: NSPoint) -> Any? { self }
    override func isAccessibilitySelectorAllowed(_ selector: Selector) -> Bool {
        if NSStringFromSelector(selector) == "accessibilityPerformPress" { return false }
        return super.isAccessibilitySelectorAllowed(selector)
    }
    nonisolated override func accessibilityPerformShowMenu() -> Bool {
        actionCalls.withLock { $0 += 1 }
        let currentMode = mode.withLock { $0 }
        guard currentMode != "reject" else { return false }
        let notice = Data("{\"kind\":\"show_menu_started\"}\n".utf8)
        notice.withUnsafeBytes { _ = Darwin.write(STDOUT_FILENO, $0.baseAddress, $0.count) }
        guard currentMode != "no-effect" else { return true }
        if currentMode == "blocking" {
            // Exposes the real synchronous tracking boundary for an owned
            // acceptance run. Cancellation still means ShowMenu was invoked;
            // NSMenu.popUp's selection Bool is not an AX action result.
            if Thread.isMainThread { MainActor.assumeIsolated { openMenu?() } }
            else { DispatchQueue.main.sync { self.openMenu?() } }
        } else {
            DispatchQueue.main.async { self.openMenu?() }
            if currentMode == "delay" { usleep(200_000) }
        }
        return true
    }
}

/// Installed only by the owned delay-selection setup command. The delay is
/// inside the actual accessibility selector, before ordinary AppKit handling.
/// Some AppKit menu bridges may bypass an NSMenuItem override; the runner must
/// observe this entry counter/notice rather than assume this fault took effect.
@MainActor
final class DelayedMenuProofItem: NSMenuItem {
    nonisolated let axPressCalls = OSAllocatedUnfairLock(initialState: 0)
    nonisolated override func accessibilityPerformPress() -> Bool {
        axPressCalls.withLock { $0 += 1 }
        let notice = Data("{\"kind\":\"selection_started\",\"entry_delay_ms\":200}\n".utf8)
        notice.withUnsafeBytes { _ = Darwin.write(STDOUT_FILENO, $0.baseAddress, $0.count) }
        usleep(200_000)
        return super.accessibilityPerformPress()
    }
}

@MainActor
final class MenuFixture: NSObject, NSMenuDelegate {
    nonisolated let commands = OSAllocatedUnfairLock(initialState: [String]())
    let window = MenuProofWindow(contentRect: NSRect(x: 240, y: 240, width: 440, height: 240),
        styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let group = MenuProofGroup(frame: NSRect(x: 0, y: 0, width: 440, height: 240))
    var button = MenuProofButton(title: "Fixture native menu", target: nil, action: nil)
    var retiredButtons: [MenuProofButton] = []
    var retiredDelayedItems: [DelayedMenuProofItem] = []
    var openerGeneration = 1
    let menu = NSMenu(title: "Fixture native menu")
    let selectionLabel = NSTextField(labelWithString: "Selection: No selection")
    let documentNavigation = MenuProofNavigation(title: "Document", target: nil, action: nil)
    let otherNavigation = MenuProofNavigation(title: "Other", target: nil, action: nil)
    let pageLabel = NSTextField(labelWithString: "Page: Document")
    var page = "Document"
    var timer: Timer?
    var occluder: NSWindow?
    var tracking = false
    var menuOpenCalls = 0, menuCloseCalls = 0, popupReturns = 0, selectionCalls = 0
    var lastPopupSelected: Bool?
    func show() {
        window.title = "Computer Use Menu Fixture — Document"
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .canJoinAllApplications]
        button.frame = NSRect(x: 80, y: 120, width: 260, height: 40); button.bezelStyle = .rounded
        group.addSubview(button); group.trigger = button; button.setAccessibilityParent(group)
        selectionLabel.frame = NSRect(x: 40, y: 72, width: 360, height: 28)
        group.addSubview(selectionLabel); group.selectionLabel = selectionLabel
        selectionLabel.setAccessibilityParent(group)
        for (navigation, x) in [(documentNavigation, 20.0), (otherNavigation, 160.0)] {
            navigation.frame = NSRect(x: x, y: 194, width: 130, height: 28)
            navigation.setButtonType(.radio); navigation.bezelStyle = .rounded
            navigation.target = navigation; navigation.action = #selector(MenuProofNavigation.activated)
            group.addSubview(navigation); navigation.setAccessibilityParent(group)
        }
        documentNavigation.state = .on; otherNavigation.state = .off
        documentNavigation.onPress = { [weak self] in self?.setPage(to: "Document") }
        otherNavigation.onPress = { [weak self] in self?.setPage(to: "Other") }
        group.navigation = [documentNavigation, otherNavigation]
        pageLabel.frame = NSRect(x: 300, y: 196, width: 130, height: 24)
        group.addSubview(pageLabel); group.pageLabel = pageLabel; pageLabel.setAccessibilityParent(group)
        group.setAccessibilityParent(window); window.contentView?.addSubview(group)
        menu.autoenablesItems = false; menu.delegate = self; menu.minimumWidth = 260
        for title in ["Fixture menu first item", "Fixture menu second item"] {
            let item = NSMenuItem(title: title, action: #selector(selectItem(_:)), keyEquivalent: "")
            item.target = self; item.isEnabled = true; menu.addItem(item)
        }
        button.selectionMenu = menu
        button.openMenu = { [weak self] in self?.showNativeMenu() }
        window.makeKeyAndOrderFront(nil); NSApplication.shared.activate(ignoringOtherApps: true)
        let frame = cgFrame(button.accessibilityFrame())
        report(["kind": "ready", "pid": ProcessInfo.processInfo.processIdentifier,
            "bundle_id": "com.lingxi.computer-user.guard-fixture", "window_id": window.windowNumber,
            "role": "AXMenuButton", "label": "Fixture native menu", "x": frame.midX, "y": frame.midY,
            "frame": box(frame), "declared_action": "AXShowMenu",
            "fault_controls": ["duplicate-items", "restore-items", "disable-first", "enable-first", "replace-opener", "move-origin-window", "delay-selection", "accept-selection"],
            "delay_selection_requires_actual_ax_entry": true,
            "delay_selection_verified": false])
        // Explicit event-tracking mode keeps the owned stdin control pipe
        // usable while popUp is inside its nested AppKit tracking loop.
        let controlTimer = Timer(timeInterval: 0.01, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.drainCommands() }
        }
        timer = controlTimer
        RunLoop.main.add(controlTimer, forMode: .common)
        RunLoop.main.add(controlTimer, forMode: .eventTracking)
        Thread { [commands] in
            let allowed = ["stats", "accept-menu", "reject-menu", "no-effect-menu", "delay-menu", "blocking-menu", "close-menu", "focus-window", "occlude", "restore", "quit",
                "duplicate-items", "restore-items", "disable-first", "enable-first", "replace-opener", "move-origin-window", "delay-selection", "accept-selection"]
            while let line = readLine(), line.utf8.count < 64, allowed.contains(line) {
                let queued = commands.withLock { pending in
                    guard pending.count < 32 else { return false }; pending.append(line); return true
                }
                guard queued else { return }
            }
            commands.withLock { $0.append("quit") }
        }.start()
    }
    func showNativeMenu() {
        guard !tracking, !button.isHidden else { return }
        // The menu itself supplies the visible UI and the native AX subtree.
        lastPopupSelected = menu.popUp(positioning: nil, at: NSPoint(x: 0, y: -4), in: button)
        popupReturns += 1
        report(["kind": "popup_returned", "selected": lastPopupSelected ?? false])
    }
    func menuWillOpen(_ menu: NSMenu) { tracking = true; menuOpenCalls += 1 }
    func menuDidClose(_ menu: NSMenu) { tracking = false; menuCloseCalls += 1 }
    func setPage(to destinationPage: String) {
        if destinationPage != "Document" { menu.cancelTrackingWithoutAnimation() }
        page = destinationPage
        window.title = "Computer Use Menu Fixture — \(page)"
        pageLabel.stringValue = "Page: \(page)"
        documentNavigation.state = page == "Document" ? .on : .off
        otherNavigation.state = page == "Other" ? .on : .off
        button.isHidden = page != "Document"
        selectionLabel.isHidden = page != "Document"
        // Reuse the same native menu and items across pages. Only the actual
        // selection callback changes their state or the displayed value.
        stats(kind: "navigation")
    }
    @objc func selectItem(_ item: NSMenuItem) {
        guard menu.items.contains(where: { $0 === item }) else { return }
        for candidate in menu.items { candidate.state = candidate === item ? .on : .off }
        selectionLabel.stringValue = "Selection: \(item.title)"
        selectionCalls += 1
        report(["kind": "menu_selection", "label": item.title, "selection_calls": selectionCalls])
    }
    func replaceOpener() {
        let old = button
        let replacement = MenuProofButton(title: old.title, target: nil, action: nil)
        replacement.frame = old.frame; replacement.bezelStyle = old.bezelStyle
        replacement.isHidden = old.isHidden; replacement.isEnabled = old.isEnabled
        let mode = old.mode.withLock { $0 }; replacement.mode.withLock { $0 = mode }
        replacement.selectionMenu = menu
        replacement.openMenu = { [weak self] in self?.showNativeMenu() }
        // Retain the detached old object so identity cannot pass through an
        // address reuse. Its counters remain part of the cumulative readback.
        old.removeFromSuperview(); old.setAccessibilityParent(nil); retiredButtons.append(old)
        group.addSubview(replacement); replacement.setAccessibilityParent(group)
        button = replacement; group.trigger = replacement; openerGeneration += 1
    }
    func configureSelectionDelay(_ delayed: Bool) {
        guard let old = menu.items.first else { return }
        if (old is DelayedMenuProofItem) == delayed { return }
        let replacement: NSMenuItem = delayed
            ? DelayedMenuProofItem(title: old.title, action: old.action, keyEquivalent: old.keyEquivalent)
            : NSMenuItem(title: old.title, action: old.action, keyEquivalent: old.keyEquivalent)
        replacement.target = old.target; replacement.isEnabled = old.isEnabled; replacement.state = old.state
        if let delayedOld = old as? DelayedMenuProofItem { retiredDelayedItems.append(delayedOld) }
        menu.removeItem(at: 0); menu.insertItem(replacement, at: 0)
    }
    func drainCommands() {
        let pending = commands.withLock { pending in let result = pending; pending.removeAll(); return result }
        for command in pending {
            switch command {
            case "accept-menu": button.mode.withLock { $0 = "accept" }
            case "reject-menu": button.mode.withLock { $0 = "reject" }
            case "no-effect-menu": button.mode.withLock { $0 = "no-effect" }
            case "delay-menu": button.mode.withLock { $0 = "delay" }
            case "blocking-menu": button.mode.withLock { $0 = "blocking" }
            case "close-menu": menu.cancelTrackingWithoutAnimation()
            case "duplicate-items":
                if menu.items.count == 2 { menu.items[1].title = menu.items[0].title }
            case "restore-items":
                if menu.items.count == 2 { menu.items[0].title = "Fixture menu first item"; menu.items[1].title = "Fixture menu second item" }
            case "disable-first": menu.items.first?.isEnabled = false
            case "enable-first": menu.items.first?.isEnabled = true
            case "replace-opener": replaceOpener()
            case "move-origin-window": window.setFrameOrigin(NSPoint(x: window.frame.minX + 32, y: window.frame.minY + 16))
            case "delay-selection": configureSelectionDelay(true)
            case "accept-selection": configureSelectionDelay(false)
            case "occlude":
                let other = MenuProofWindow(contentRect: window.contentRect(forFrameRect: window.frame),
                    styleMask: [.titled], backing: .buffered, defer: false)
                other.title = "Owned menu occluder"; other.isReleasedWhenClosed = false
                other.collectionBehavior = window.collectionBehavior
                other.makeKeyAndOrderFront(nil); occluder = other
            case "restore": occluder?.orderOut(nil); occluder = nil; window.makeKeyAndOrderFront(nil)
            case "focus-window": window.makeKeyAndOrderFront(nil); NSApplication.shared.activate(ignoringOtherApps: true)
            case "quit": menu.cancelTrackingWithoutAnimation(); NSApplication.shared.terminate(nil)
            default: break
            }
            stats(kind: command)
        }
    }
    func cgFrame(_ frame: NSRect) -> CGRect {
        CGRect(x: frame.minX, y: CGDisplayBounds(CGMainDisplayID()).height - frame.maxY,
            width: frame.width, height: frame.height)
    }
    func box(_ frame: CGRect) -> [String: Double] {
        ["x": frame.minX, "y": frame.minY, "width": frame.width, "height": frame.height]
    }
    func stats(kind: String) {
        let pid = ProcessInfo.processInfo.processIdentifier
        let windows = (CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []).filter {
            ($0[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value == pid &&
            ($0[kCGWindowNumber as String] as? NSNumber)?.intValue != window.windowNumber &&
            (($0[kCGWindowAlpha as String] as? NSNumber)?.doubleValue ?? 0) > 0
        }
        let nativeWindows: [[String: Any]] = windows.compactMap {
            guard let bounds = $0[kCGWindowBounds as String] as? [String: Any],
                  let frame = CGRect(dictionaryRepresentation: bounds as CFDictionary), frame.width > 0, frame.height > 0 else { return nil }
            return ["window_id": ($0[kCGWindowNumber as String] as? NSNumber)?.intValue ?? -1,
                "layer": ($0[kCGWindowLayer as String] as? NSNumber)?.intValue ?? -1, "frame": box(frame)]
        }
        // These frames come from AppKit's native menu items, never an invented
        // accessibility menu tree. A fresh external AX source/screenshot is
        // still required for end-to-end verification and authority binding.
        let items: [[String: Any]] = menu.items.map {
            ["label": $0.title, "role": $0.accessibilityRole()?.rawValue ?? "unavailable",
                "frame": box(cgFrame($0.accessibilityFrame())), "state": $0.state.rawValue, "enabled": $0.isEnabled]
        }
        let visibleItems = menu.items.filter { item in
            let frame = cgFrame(item.accessibilityFrame())
            guard frame.width > 0, frame.height > 0 else { return false }
            return windows.contains {
                guard let bounds = $0[kCGWindowBounds as String] as? [String: Any],
                      let windowFrame = CGRect(dictionaryRepresentation: bounds as CFDictionary) else { return false }
                return windowFrame.contains(CGPoint(x: frame.midX, y: frame.midY))
            }
        }
        let allButtons = [button] + retiredButtons
        let delayedItems = menu.items.compactMap { $0 as? DelayedMenuProofItem } + retiredDelayedItems
        let axPressCalls = delayedItems.reduce(0) { $0 + $1.axPressCalls.withLock { $0 } }
        report(["kind": kind, "action_calls": allButtons.reduce(0) { $0 + $1.actionCalls.withLock { $0 } },
            "current_opener_action_calls": button.actionCalls.withLock { $0 },
            "opener_generation": openerGeneration, "retired_opener_count": retiredButtons.count,
            "opener_frame": box(cgFrame(button.accessibilityFrame())), "origin_window_frame": box(cgFrame(window.frame)),
            "selection_ax_press_calls": axPressCalls,
            "delay_selection_mode": menu.items.first is DelayedMenuProofItem,
            "delay_selection_entry_observed": axPressCalls > 0,
            "delay_selection_verified": false,
            "page": page, "page_label": pageLabel.stringValue,
            "navigation_calls": documentNavigation.calls.withLock { $0 } + otherNavigation.calls.withLock { $0 },
            "document_navigation_calls": documentNavigation.calls.withLock { $0 },
            "other_navigation_calls": otherNavigation.calls.withLock { $0 },
            "menu_will_open_calls": menuOpenCalls, "menu_did_close_calls": menuCloseCalls,
            "popup_returns": popupReturns, "selection_calls": selectionCalls, "tracking": tracking,
            "selected_label": menu.items.first(where: { $0.state == .on })?.title as Any? ?? NSNull(),
            "selected_value": button.accessibilityValue() as Any? ?? NSNull(),
            "selection_label": selectionLabel.stringValue,
            "item_states": menu.items.map { $0.state.rawValue },
            "native_menu_windows": nativeWindows, "native_menu_ax_items": items,
            "visible_native_item_count": visibleItems.count,
            "menu_visibility_verified": tracking && visibleItems.count == menu.items.count && !nativeWindows.isEmpty,
            "window_id": window.windowNumber, "window_key": window.isKeyWindow, "app_active": NSApplication.shared.isActive])
    }
    func report(_ value: [String: Any]) {
        guard var data = try? JSONSerialization.data(withJSONObject: value) else { return }
        data.append(10); FileHandle.standardOutput.write(data)
    }
}

@main
struct MenuFixtureMain {
    @MainActor static func main() {
        let app = NSApplication.shared, fixture = MenuFixture()
        app.setActivationPolicy(.regular); app.finishLaunching(); fixture.show()
        withExtendedLifetime(fixture) { app.run() }
    }
}
