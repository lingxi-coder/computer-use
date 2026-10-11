// Metadata-only inventory. A shortcut candidate is not Save authority.
import ApplicationServices
import Foundation
import CryptoKit

struct OwnedSaveMenuCandidate: Encodable {
    let enabled: Bool?, press_supported: Bool, identifier_present: Bool
    let title_is_registered_save_word: Bool
    let identifier_sha256: String?
}

struct OwnedSaveMenuInventory: Encodable {
    let tree_complete: Bool, node_count: Int, command_s_candidates: [OwnedSaveMenuCandidate]
    let failure_reason: String?
    let file_menu_selected: Bool
    let failed_children_role: String?, failed_children_error: Int32?
    let direct_file_items_complete: Bool, direct_command_s_count: Int
}

func readOwnedSaveMenu(application: AXUIElement, pid: Int32) -> OwnedSaveMenuInventory {
    let deadline = DispatchTime.now().uptimeNanoseconds + 300_000_000
    func owned(_ element: AXUIElement) -> Bool {
        var owner: pid_t = 0
        return AXUIElementGetPid(element, &owner) == .success && owner == pid
    }
    func read(_ element: AXUIElement, _ attribute: String) -> (CFTypeRef?, AXError) {
        let now = DispatchTime.now().uptimeNanoseconds
        guard now < deadline, owned(element),
              AXUIElementSetMessagingTimeout(element, Float(min(0.04, Double(deadline - now) / 1_000_000_000))) == .success
        else { return (nil, .cannotComplete) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, attribute as CFString, &value)
        guard DispatchTime.now().uptimeNanoseconds < deadline else { return (nil, .cannotComplete) }
        return (value, error)
    }
    let menu = read(application, kAXMenuBarAttribute)
    guard menu.1 == .success, let rawMenu = menu.0, CFGetTypeID(rawMenu) == AXUIElementGetTypeID()
    else { return OwnedSaveMenuInventory(tree_complete: false, node_count: 0,
        command_s_candidates: [], failure_reason: "menu_bar_unavailable", file_menu_selected: false,
        failed_children_role: nil, failed_children_error: nil,
        direct_file_items_complete: false, direct_command_s_count: 0) }
    let menuBar = unsafeDowncast(rawMenu, to: AXUIElement.self)
    let top = read(menuBar, kAXChildrenAttribute)
    var fileMenus: [AXUIElement] = []
    if top.1 == .success, let raw = top.0, CFGetTypeID(raw) == CFArrayGetTypeID(),
       let items = raw as? [AXUIElement], items.count <= 32 {
        for item in items {
            let role = read(item, kAXRoleAttribute), title = read(item, kAXTitleAttribute)
            if role.1 == .success && role.0 as? String == kAXMenuBarItemRole,
               title.1 == .success && ["File", "文件"].contains(title.0 as? String ?? "") {
                fileMenus.append(item)
            }
        }
    }
    let fileSelected = fileMenus.count == 1
    var directComplete = false, directCount = 0
    if fileSelected {
        let childRead = read(fileMenus[0], kAXChildrenAttribute)
        if childRead.1 == .success, let raw = childRead.0, CFGetTypeID(raw) == CFArrayGetTypeID(),
           let roots = raw as? [AXUIElement], roots.count == 1, owned(roots[0]) {
            let role = read(roots[0], kAXRoleAttribute)
            let itemsRead = read(roots[0], kAXChildrenAttribute)
            if role.1 == .success && role.0 as? String == kAXMenuRole,
               itemsRead.1 == .success, let rawItems = itemsRead.0, CFGetTypeID(rawItems) == CFArrayGetTypeID(),
               let items = rawItems as? [AXUIElement], items.count <= 128,
               items.count == CFArrayGetCount(unsafeDowncast(rawItems, to: CFArray.self)),
               items.allSatisfy({ owned($0) }) {
                directComplete = true
                for item in items {
                    let role = read(item, kAXRoleAttribute)
                    guard role.1 == .success && role.0 as? String == kAXMenuItemRole else {
                        directComplete = false; break
                    }
                    let character = read(item, kAXMenuItemCmdCharAttribute)
                    if character.1 == .success && ["s", "S"].contains(character.0 as? String ?? "") {
                        let modifiers = read(item, kAXMenuItemCmdModifiersAttribute)
                        guard modifiers.1 == .success, let value = modifiers.0,
                              CFGetTypeID(value) == CFNumberGetTypeID(), let number = value as? NSNumber
                        else { directComplete = false; break }
                        if number.int64Value == 0 { directCount += 1 }
                    } else if character.1 != .success && character.1 != .attributeUnsupported && character.1 != .noValue {
                        directComplete = false; break
                    }
                }
            }
        }
    }
    var queue: [(AXUIElement, Int)] = [(fileSelected ? fileMenus[0] : menuBar, 0)]
    var visited: [AXUIElement] = [], candidates: [OwnedSaveMenuCandidate] = []
    var complete = true, cursor = 0
    var failure: String?
    var failedRole: String?, failedError: Int32?
    while cursor < queue.count {
        guard queue.count <= 512, DispatchTime.now().uptimeNanoseconds < deadline else {
            complete = false; failure = "inventory_budget_exceeded"; break
        }
        let (element, depth) = queue[cursor]; cursor += 1
        guard depth <= 12, owned(element), !visited.contains(where: { CFEqual($0, element) })
        else { complete = false; failure = "ownership_depth_or_cycle"; break }
        visited.append(element)
        let role = read(element, kAXRoleAttribute)
        guard role.1 == .success, let roleName = role.0 as? String, !roleName.isEmpty else {
            complete = false; failure = "role_unavailable"; break
        }
        if roleName == kAXMenuItemRole {
            let character = read(element, kAXMenuItemCmdCharAttribute)
            let modifiers = read(element, kAXMenuItemCmdModifiersAttribute)
            if character.1 == .success && ["s", "S"].contains(character.0 as? String ?? ""),
               modifiers.1 == .success, let number = modifiers.0,
               CFGetTypeID(number) == CFNumberGetTypeID(), (number as? NSNumber)?.int64Value == 0 {
                let enabledRead = read(element, kAXEnabledAttribute)
                let enabled = enabledRead.1 == .success && enabledRead.0.map({ CFGetTypeID($0) == CFBooleanGetTypeID() }) == true
                    ? enabledRead.0 as? Bool : nil
                let identifier = read(element, kAXIdentifierAttribute)
                let title = read(element, kAXTitleAttribute)
                var actions: CFArray?
                let actionError = DispatchTime.now().uptimeNanoseconds < deadline
                    ? AXUIElementCopyActionNames(element, &actions) : .cannotComplete
                let press = actionError == .success && DispatchTime.now().uptimeNanoseconds < deadline
                    && (actions as? [String])?.contains(kAXPressAction) == true
                candidates.append(OwnedSaveMenuCandidate(enabled: enabled, press_supported: press,
                    identifier_present: identifier.1 == .success && (identifier.0 as? String)?.isEmpty == false,
                    title_is_registered_save_word: title.1 == .success
                        && ["Save", "存储", "保存"].contains(title.0 as? String ?? ""),
                    identifier_sha256: (identifier.1 == .success ? identifier.0 as? String : nil).map {
                        SHA256.hash(data: Data($0.utf8)).map { String(format: "%02x", $0) }.joined()
                    }))
            }
        }
        let children = read(element, kAXChildrenAttribute)
        if children.1 == .noValue && children.0 == nil { continue }
        guard children.1 == .success, let raw = children.0, CFGetTypeID(raw) == CFArrayGetTypeID(),
              let elements = raw as? [AXUIElement], elements.count == CFArrayGetCount(unsafeDowncast(raw, to: CFArray.self)),
              elements.allSatisfy({ CFGetTypeID($0) == AXUIElementGetTypeID() }), queue.count + elements.count <= 512
        else {
            complete = false; failure = "children_unavailable"
            failedRole = [kAXMenuItemRole, kAXMenuRole, kAXMenuBarItemRole].contains(roleName) ? roleName : "other"
            failedError = children.1.rawValue
            break
        }
        queue += elements.map { ($0, depth + 1) }
    }
    return OwnedSaveMenuInventory(tree_complete: complete && cursor == queue.count,
        node_count: visited.count, command_s_candidates: candidates, failure_reason: failure,
        file_menu_selected: fileSelected, failed_children_role: failedRole, failed_children_error: failedError,
        direct_file_items_complete: directComplete, direct_command_s_count: directCount)
}
