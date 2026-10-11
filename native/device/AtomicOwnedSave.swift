import AppKit
import ApplicationServices
import Darwin
import Foundation

struct OwnedSaveMenuItemFacts: Sendable, Equatable {
    let role: String
    let commandCharacter: String?
    let commandModifiers: Int64?
    let title: String?
    let enabled: Bool?
    let pressSupported: Bool
    let identifier: String?
}

func ownedSaveMenuItemIndex(_ items: [OwnedSaveMenuItemFacts]) -> Int? {
    guard !items.isEmpty, items.count <= 128,
          items.allSatisfy({ $0.role == kAXMenuItemRole }) else { return nil }
    let shortcuts = items.enumerated().filter {
        $0.element.commandCharacter == "s" || $0.element.commandCharacter == "S"
    }
    guard shortcuts.allSatisfy({ $0.element.commandModifiers != nil }) else { return nil }
    let commandOnly = shortcuts.filter { $0.element.commandModifiers == 0 }
    guard commandOnly.count == 1,
          let (index, item) = commandOnly.first,
          ["Save", "存储", "保存"].contains(item.title ?? ""),
          item.enabled == true,
          item.pressSupported,
          let identifier = item.identifier,
          !identifier.isEmpty else { return nil }
    return index
}

func ownedDocumentSaveValueIsValid(_ value: String) -> Bool {
    value.utf8.count <= 2_048 && !value.contains("\0")
}

private struct OwnedSaveMenuTarget {
    let menuBar: AXUIElement
    let fileItem: AXUIElement
    let menu: AXUIElement
    let item: AXUIElement
    let identifier: String
}

private func ownedSaveElementBelongsTo(_ element: AXUIElement, pid: Int32) -> Bool {
    var owner: pid_t = 0
    return AXUIElementGetPid(element, &owner) == .success && owner == pid
}

private func ownedSaveRead(
    _ element: AXUIElement,
    _ attribute: String,
    pid: Int32?,
    deadlineMS: UInt64
) -> (CFTypeRef?, AXError)? {
    let now = monotonicMS()
    guard now < deadlineMS,
          pid.map({ ownedSaveElementBelongsTo(element, pid: $0) }) ?? true,
          AXUIElementSetMessagingTimeout(
              element,
              Float(min(25, deadlineMS - now)) / 1000
          ) == .success else { return nil }
    var value: CFTypeRef?
    let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
    guard monotonicMS() < deadlineMS else { return nil }
    return (value, error)
}

private func ownedSaveString(
    _ element: AXUIElement,
    _ attribute: String,
    pid: Int32,
    deadlineMS: UInt64
) -> String? {
    guard let (raw, error) = ownedSaveRead(element, attribute, pid: pid, deadlineMS: deadlineMS),
          error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID() else { return nil }
    let maximumUnits: Int
    switch attribute {
    case kAXRoleAttribute: maximumUnits = 64
    case kAXTitleAttribute: maximumUnits = 1_024
    case kAXMenuItemCmdCharAttribute: maximumUnits = 2
    case kAXIdentifierAttribute: maximumUnits = 256
    case kAXDocumentAttribute: maximumUnits = 4_096
    default: maximumUnits = 1_024
    }
    guard CFStringGetLength(unsafeDowncast(raw, to: CFString.self)) <= maximumUnits else { return nil }
    return raw as? String
}

private func ownedSaveCanonicalPath(_ path: String) -> String? {
    guard let resolved = path.withCString({ realpath($0, nil) }) else { return nil }
    defer { free(resolved) }
    return String(cString: resolved)
}

private func ownedSaveDocumentPath(
    _ element: AXUIElement,
    pid: Int32,
    deadlineMS: UInt64
) -> String? {
    guard let (raw, error) = ownedSaveRead(element, kAXDocumentAttribute, pid: pid, deadlineMS: deadlineMS),
          error == .success, let raw else { return nil }
    let url: URL?
    if CFGetTypeID(raw) == CFURLGetTypeID() {
        let cfURL = unsafeDowncast(raw, to: CFURL.self)
        let text = CFURLGetString(cfURL)
        guard CFStringGetLength(text) <= 4_096 else { return nil }
        url = cfURL as URL
    } else if CFGetTypeID(raw) == CFStringGetTypeID() {
        let cfString = unsafeDowncast(raw, to: CFString.self)
        guard CFStringGetLength(cfString) <= 4_096,
              let text = raw as? String else { return nil }
        url = text.hasPrefix("/") ? URL(fileURLWithPath: text) : URL(string: text)
    } else {
        return nil
    }
    guard let url, url.isFileURL, url.query == nil, url.fragment == nil,
          url.user == nil, url.password == nil, url.port == nil,
          (url.host == nil || url.host?.isEmpty == true
            || url.host?.caseInsensitiveCompare("localhost") == .orderedSame),
          let canonical = ownedSaveCanonicalPath(url.path), monotonicMS() < deadlineMS else { return nil }
    return canonical
}

private func ownedSaveCurrentValueMatches(
    _ receiver: AXUIElement,
    expectedValue: String,
    pid: Int32,
    deadlineMS: UInt64
) -> Bool {
    guard ownedDocumentSaveValueIsValid(expectedValue),
          let (raw, error) = ownedSaveRead(receiver, kAXValueAttribute, pid: pid, deadlineMS: deadlineMS),
          error == .success, let raw, CFGetTypeID(raw) == CFStringGetTypeID(),
          CFStringGetLength(unsafeDowncast(raw, to: CFString.self)) <= 2_048,
          let currentValue = raw as? String,
          currentValue.utf8.count <= 2_048,
          currentValue == expectedValue,
          monotonicMS() < deadlineMS else { return false }
    return true
}

private func ownedSaveRole(_ element: AXUIElement, pid: Int32, deadlineMS: UInt64) -> String? {
    ownedSaveString(element, kAXRoleAttribute, pid: pid, deadlineMS: deadlineMS)
}

private func ownedSaveParent(
    _ element: AXUIElement,
    expected: AXUIElement,
    pid: Int32,
    deadlineMS: UInt64
) -> Bool {
    guard let (raw, error) = ownedSaveRead(element, kAXParentAttribute, pid: pid, deadlineMS: deadlineMS),
          error == .success, let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return false }
    return CFEqual(unsafeDowncast(raw, to: AXUIElement.self), expected)
}

private func ownedSaveChildren(
    _ element: AXUIElement,
    maximum: Int,
    pid: Int32,
    deadlineMS: UInt64
) -> [AXUIElement]? {
    guard let (raw, error) = ownedSaveRead(element, kAXChildrenAttribute, pid: pid, deadlineMS: deadlineMS),
          error == .success, let raw, CFGetTypeID(raw) == CFArrayGetTypeID(),
          let children = raw as? [AXUIElement], children.count <= maximum,
          children.count == CFArrayGetCount(unsafeDowncast(raw, to: CFArray.self)),
          children.allSatisfy({
              CFGetTypeID($0) == AXUIElementGetTypeID()
                  && ownedSaveElementBelongsTo($0, pid: pid)
          }) else { return nil }
    return children
}

private func ownedSaveMenuItemFacts(
    _ item: AXUIElement,
    pid: Int32,
    deadlineMS: UInt64
) -> OwnedSaveMenuItemFacts? {
    guard ownedSaveElementBelongsTo(item, pid: pid),
          ownedSaveRole(item, pid: pid, deadlineMS: deadlineMS) == kAXMenuItemRole,
          let (characterRaw, characterError) = ownedSaveRead(
              item, kAXMenuItemCmdCharAttribute, pid: pid, deadlineMS: deadlineMS
          ) else { return nil }
    let character: String?
    if characterError == .success {
        guard let characterRaw, CFGetTypeID(characterRaw) == CFStringGetTypeID(),
              CFStringGetLength(unsafeDowncast(characterRaw, to: CFString.self)) <= 2,
              let value = characterRaw as? String else { return nil }
        character = value
    } else if characterError == .attributeUnsupported || characterError == .noValue {
        character = nil
    } else {
        return nil
    }
    guard character == "s" || character == "S" else {
        return OwnedSaveMenuItemFacts(
            role: kAXMenuItemRole,
            commandCharacter: character,
            commandModifiers: nil,
            title: nil,
            enabled: nil,
            pressSupported: false,
            identifier: nil
        )
    }
    guard let (modifierRaw, modifierError) = ownedSaveRead(
        item, kAXMenuItemCmdModifiersAttribute, pid: pid, deadlineMS: deadlineMS
    ), modifierError == .success, let modifierRaw,
       CFGetTypeID(modifierRaw) == CFNumberGetTypeID(),
       let modifierNumber = modifierRaw as? NSNumber else { return nil }
    let modifier = modifierNumber.int64Value
    let title = ownedSaveString(item, kAXTitleAttribute, pid: pid, deadlineMS: deadlineMS)
    let enabled: Bool?
    if let (enabledRaw, enabledError) = ownedSaveRead(item, kAXEnabledAttribute, pid: pid, deadlineMS: deadlineMS),
       enabledError == .success, let enabledRaw,
       CFGetTypeID(enabledRaw) == CFBooleanGetTypeID() {
        enabled = (enabledRaw as? NSNumber)?.boolValue
    } else {
        enabled = nil
    }
    var actionsRaw: CFArray?
    let actionsError = monotonicMS() < deadlineMS
        ? AXUIElementCopyActionNames(item, &actionsRaw)
        : .cannotComplete
    let pressSupported: Bool
    if actionsError == .success, monotonicMS() < deadlineMS,
       let actionsRaw, CFGetTypeID(actionsRaw) == CFArrayGetTypeID(),
       CFArrayGetCount(actionsRaw) <= 64 {
        var containsPress = false
        var validActions = true
        for index in 0..<CFArrayGetCount(actionsRaw) {
            let pointer = CFArrayGetValueAtIndex(actionsRaw, index)
            let rawAction = unsafeBitCast(pointer, to: CFTypeRef.self)
            guard CFGetTypeID(rawAction) == CFStringGetTypeID(),
                  CFStringGetLength(unsafeDowncast(rawAction, to: CFString.self)) <= 64,
                  let action = rawAction as? String else {
                validActions = false
                break
            }
            if action == kAXPressAction { containsPress = true }
        }
        pressSupported = validActions && containsPress && monotonicMS() < deadlineMS
    } else {
        pressSupported = false
    }
    let identifier = ownedSaveString(item, kAXIdentifierAttribute, pid: pid, deadlineMS: deadlineMS)
    return OwnedSaveMenuItemFacts(
        role: kAXMenuItemRole,
        commandCharacter: character,
        commandModifiers: modifier,
        title: title,
        enabled: enabled,
        pressSupported: pressSupported,
        identifier: identifier
    )
}

private func ownedSaveMenuTarget(
    application: AXUIElement,
    pid: Int32,
    deadlineMS: UInt64
) -> OwnedSaveMenuTarget? {
    guard ownedSaveElementBelongsTo(application, pid: pid),
          let (menuBarRaw, menuBarError) = ownedSaveRead(
              application, kAXMenuBarAttribute, pid: pid, deadlineMS: deadlineMS
          ), menuBarError == .success, let menuBarRaw,
          CFGetTypeID(menuBarRaw) == AXUIElementGetTypeID() else { return nil }
    let menuBar = unsafeDowncast(menuBarRaw, to: AXUIElement.self)
    guard ownedSaveRole(menuBar, pid: pid, deadlineMS: deadlineMS) == kAXMenuBarRole,
          ownedSaveParent(menuBar, expected: application, pid: pid, deadlineMS: deadlineMS),
          let menuBarItems = ownedSaveChildren(menuBar, maximum: 32, pid: pid, deadlineMS: deadlineMS) else { return nil }
    var fileItems: [AXUIElement] = []
    for menuBarItem in menuBarItems {
        guard ownedSaveRole(menuBarItem, pid: pid, deadlineMS: deadlineMS) == kAXMenuBarItemRole,
              ownedSaveParent(menuBarItem, expected: menuBar, pid: pid, deadlineMS: deadlineMS),
              let title = ownedSaveString(menuBarItem, kAXTitleAttribute, pid: pid, deadlineMS: deadlineMS) else {
            return nil
        }
        if ["File", "文件"].contains(title) { fileItems.append(menuBarItem) }
    }
    guard fileItems.count == 1, let fileItem = fileItems.first,
          let fileChildren = ownedSaveChildren(fileItem, maximum: 8, pid: pid, deadlineMS: deadlineMS),
          fileChildren.count == 1, let menu = fileChildren.first,
          ownedSaveRole(menu, pid: pid, deadlineMS: deadlineMS) == kAXMenuRole,
          ownedSaveParent(menu, expected: fileItem, pid: pid, deadlineMS: deadlineMS),
          let items = ownedSaveChildren(menu, maximum: 128, pid: pid, deadlineMS: deadlineMS) else { return nil }
    var facts: [OwnedSaveMenuItemFacts] = []
    facts.reserveCapacity(items.count)
    for item in items {
        guard ownedSaveRole(item, pid: pid, deadlineMS: deadlineMS) == kAXMenuItemRole,
              ownedSaveParent(item, expected: menu, pid: pid, deadlineMS: deadlineMS),
              let itemFacts = ownedSaveMenuItemFacts(item, pid: pid, deadlineMS: deadlineMS) else { return nil }
        facts.append(itemFacts)
    }
    guard let selectedIndex = ownedSaveMenuItemIndex(facts),
          items.indices.contains(selectedIndex),
          let identifier = facts[selectedIndex].identifier else { return nil }
    return OwnedSaveMenuTarget(
        menuBar: menuBar,
        fileItem: fileItem,
        menu: menu,
        item: items[selectedIndex],
        identifier: identifier
    )
}

private func ownedSaveReceiverStillMatches(
    registration: NativeOwnedDocumentRegistration,
    application: AXUIElement,
    receiver: AXUIElement,
    expectedValue: String,
    pid: Int32,
    deadlineMS: UInt64
) -> Bool {
    guard monotonicMS() < deadlineMS,
          registration.verifyLiveIdentity(checkDocumentHash: false),
          DeviceProcessStamp.read(pid)
              == DeviceProcessStamp(seconds: registration.start_seconds, micros: registration.start_micros),
          let running = NSRunningApplication(processIdentifier: pid), !running.isTerminated,
          running.bundleIdentifier == registration.bundle_id,
          ownedSaveElementBelongsTo(application, pid: pid),
          ownedSaveElementBelongsTo(receiver, pid: pid) else { return false }
    let freshApplication = AXUIElementCreateApplication(pid)
    let system = AXUIElementCreateSystemWide()
    guard CFEqual(application, freshApplication),
          let (focusedApplicationRaw, appError) = ownedSaveRead(
              system, kAXFocusedApplicationAttribute, pid: nil, deadlineMS: deadlineMS
          ), appError == .success, let focusedApplicationRaw,
          CFGetTypeID(focusedApplicationRaw) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(focusedApplicationRaw, to: AXUIElement.self), application),
          let (applicationFocusRaw, applicationFocusError) = ownedSaveRead(
              application, kAXFocusedUIElementAttribute, pid: pid, deadlineMS: deadlineMS
          ), applicationFocusError == .success, let applicationFocusRaw,
          CFGetTypeID(applicationFocusRaw) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(applicationFocusRaw, to: AXUIElement.self), receiver),
          let (systemFocusRaw, systemFocusError) = ownedSaveRead(
              system, kAXFocusedUIElementAttribute, pid: nil, deadlineMS: deadlineMS
          ), systemFocusError == .success, let systemFocusRaw,
          CFGetTypeID(systemFocusRaw) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(systemFocusRaw, to: AXUIElement.self), receiver),
          let (windowRaw, windowError) = ownedSaveRead(
              application, kAXFocusedWindowAttribute, pid: pid, deadlineMS: deadlineMS
          ), windowError == .success, let windowRaw,
          CFGetTypeID(windowRaw) == AXUIElementGetTypeID(),
          let (topLevelRaw, topLevelError) = ownedSaveRead(
              receiver, kAXTopLevelUIElementAttribute, pid: pid, deadlineMS: deadlineMS
          ), topLevelError == .success, let topLevelRaw,
          CFGetTypeID(topLevelRaw) == AXUIElementGetTypeID(),
          CFEqual(unsafeDowncast(windowRaw, to: AXUIElement.self),
                  unsafeDowncast(topLevelRaw, to: AXUIElement.self)),
          monotonicMS() < deadlineMS else { return false }
    let focusedWindow = unsafeDowncast(windowRaw, to: AXUIElement.self)
    return ownedSaveDocumentPath(focusedWindow, pid: pid, deadlineMS: deadlineMS)
            == registration.document_path
        && ownedSaveCurrentValueMatches(
            receiver,
            expectedValue: expectedValue,
            pid: pid,
            deadlineMS: deadlineMS
        )
}

func verifiedOwnedSaveTarget(
    registration: NativeOwnedDocumentRegistration,
    receiver: AXUIElement,
    expectedValue: String,
    deadlineMS: UInt64
) -> AXUIElement? {
    let pid = Int32(registration.pid)
    guard let application = NSRunningApplication(processIdentifier: pid),
          !application.isTerminated, application.bundleIdentifier == registration.bundle_id else { return nil }
    let axApplication = AXUIElementCreateApplication(pid)
    guard ownedSaveReceiverStillMatches(
              registration: registration,
              application: axApplication,
              receiver: receiver,
              expectedValue: expectedValue,
              pid: pid,
              deadlineMS: deadlineMS
          ),
          let first = ownedSaveMenuTarget(application: axApplication, pid: pid, deadlineMS: deadlineMS),
          ownedSaveReceiverStillMatches(
              registration: registration,
              application: axApplication,
              receiver: receiver,
              expectedValue: expectedValue,
              pid: pid,
              deadlineMS: deadlineMS
          ),
          let refreshed = ownedSaveMenuTarget(application: axApplication, pid: pid, deadlineMS: deadlineMS),
          CFEqual(first.menuBar, refreshed.menuBar),
          CFEqual(first.fileItem, refreshed.fileItem),
          CFEqual(first.menu, refreshed.menu),
          CFEqual(first.item, refreshed.item),
          first.identifier == refreshed.identifier,
          ownedSaveReceiverStillMatches(
              registration: registration,
              application: axApplication,
              receiver: receiver,
              expectedValue: expectedValue,
              pid: pid,
              deadlineMS: deadlineMS
          ),
          registration.verifyLiveIdentity(checkDocumentHash: false),
          monotonicMS() < deadlineMS,
          ownedSaveElementBelongsTo(refreshed.item, pid: pid),
          ownedSaveParent(refreshed.item, expected: refreshed.menu, pid: pid, deadlineMS: deadlineMS) else {
        return nil
    }
    let now = monotonicMS()
    guard now < deadlineMS,
          AXUIElementSetMessagingTimeout(refreshed.item, Float(min(25, deadlineMS - now)) / 1000) == .success else {
        return nil
    }
    return refreshed.item
}
