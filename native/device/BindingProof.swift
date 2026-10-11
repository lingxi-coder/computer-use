import ApplicationServices
import CryptoKit
import Foundation

/// The fp1 wire shape is shared with Rust InstanceFingerprint. No AX refs or
/// source tree are transported. Nullable fields remain explicit in the digest.
struct NativeBindingFingerprint: Codable, Sendable, Equatable {
    var app: String
    var app_pid: UInt32
    var window_id: String
    var ancestor_roles: [String]
    var group: String?
    var role: String
    var subrole: String?
    var label: String
    var sibling_index: UInt32
    var frame: [Int64]
    var scroll_container: String?
    static let keys: Set<String> = ["app", "app_pid", "window_id", "ancestor_roles", "group", "role", "subrole",
        "label", "sibling_index", "frame", "scroll_container"]
    private enum CodingKeys: String, CodingKey { case app, app_pid, window_id, ancestor_roles, group, role, subrole, label, sibling_index, frame, scroll_container }
    var isRegisteredSheetBinding: Bool {
        app == "com.lingxi.computer-user.mvp-fixture" && ancestor_roles.first == kAXSheetRole &&
            ancestor_roles.dropFirst().allSatisfy {
                ![kAXWindowRole, kAXSheetRole, "AXDialog", "AXPopover", kAXApplicationRole].contains($0)
            }
    }
    func encode(to encoder: any Encoder) throws {
        var fields = encoder.container(keyedBy: CodingKeys.self)
        try fields.encode(app, forKey: .app); try fields.encode(app_pid, forKey: .app_pid)
        try fields.encode(window_id, forKey: .window_id); try fields.encode(ancestor_roles, forKey: .ancestor_roles)
        try fields.encode(group, forKey: .group); try fields.encode(role, forKey: .role)
        try fields.encode(subrole, forKey: .subrole); try fields.encode(label, forKey: .label)
        try fields.encode(sibling_index, forKey: .sibling_index); try fields.encode(frame, forKey: .frame)
        try fields.encode(scroll_container, forKey: .scroll_container)
    }
    var valid: Bool {
        !app.isEmpty && app.utf8.count <= 256 && app_pid > 0 && !window_id.isEmpty && window_id.utf8.count <= 64 &&
        ((role == kAXWindowRole && subrole == kAXStandardWindowSubrole && ancestor_roles.isEmpty && group == nil && sibling_index == 0) ||
         (role != kAXWindowRole && !ancestor_roles.isEmpty && ancestor_roles.count <= 32 &&
          (ancestor_roles.first == kAXWindowRole || isRegisteredSheetBinding))) &&
        ancestor_roles.allSatisfy { !$0.isEmpty && $0.utf8.count <= 64 } &&
        role.utf8.count <= 64 && !role.isEmpty && (subrole == nil || subrole!.utf8.count <= 128) &&
        label.utf8.count <= 512 && label == bindingLabel(label) &&
        (group == nil || (group!.utf8.count <= 512 && group == bindingLabel(group!))) &&
        sibling_index < 256 && frame.count == 4 && frame.allSatisfy { (-100_000...100_000).contains($0) } &&
        frame[2] > 0 && frame[3] > 0 && scroll_container == nil
    }
    var canonical: Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        func string(_ value: String) -> String { String(decoding: try! encoder.encode(value), as: UTF8.self) }
        func optional(_ value: String?) -> String { value.map(string) ?? "null" }
        let fields = ["\"app\":\(string(app))", "\"app_pid\":\(app_pid)", "\"window_id\":\(string(window_id))",
            "\"ancestor_roles\":[\(ancestor_roles.map(string).joined(separator: ","))]", "\"group\":\(optional(group))",
            "\"role\":\(string(role))", "\"subrole\":\(optional(subrole))", "\"label\":\(string(label))",
            "\"sibling_index\":\(sibling_index)", "\"frame\":[\(frame.map(String.init).joined(separator: ","))]",
            "\"scroll_container\":\(optional(scroll_container))"]
        return Data(("{" + fields.joined(separator: ",") + "}").utf8)
    }
    var digest: String { "fp1:" + SHA256.hash(data: canonical).map { String(format: "%02x", $0) }.joined() }
}

/// Rust str::split_whitespace uses Unicode White_Space, not Foundation's
/// broader whitespace set. An explicit POSIX locale preserves Rust's contextual
/// final-sigma mapping; Swift's plain lowercased() maps it differently.
func bindingLabel(_ text: String) -> String {
    func whitespace(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x09...0x0D, 0x20, 0x85, 0xA0, 0x1680, 0x2000...0x200A, 0x2028, 0x2029, 0x202F, 0x205F, 0x3000: true
        default: false
        }
    }
    return text.unicodeScalars.split(whereSeparator: whitespace).map {
        String(String.UnicodeScalarView($0))
    }.joined(separator: " ").lowercased(with: Locale(identifier: "en_US_POSIX"))
}

/// Worker-local facts from one actual AX walk. AX refs never cross the serial
/// device worker or appear in a proof. This is a current relationship sample,
/// not a mapping from a Source ObjectRef to an AX proxy.
struct NativeBindingPathNode {
    let element: AXUIElement
    let parent: AXUIElement?
    let siblingIndex: UInt32?
    let role: String
    let subrole: String?
    let label: String
    let group: String?
    let frame: HitRect
    let scroll: NativeBindingScrollContext?
}

struct NativeBindingSiblingNode {
    let element: AXUIElement
    let role: String
    let subrole: String?
    let label: String
    let frame: HitRect
}

struct NativeBindingScrollAxisContext {
    let element: AXUIElement
    let value: Double
    let minimum: Double?
    let maximum: Double?
}

struct NativeBindingScrollContext {
    let horizontal: NativeBindingScrollAxisContext?
    let vertical: NativeBindingScrollAxisContext?
}

struct NativeBindingTableColumnEntry {
    let element: AXUIElement
    let axIndex: UInt8
    let header: AXUIElement
}

struct NativeBindingTableColumnGraph {
    let table: AXUIElement
    let ancestryIndex: UInt8
    let columns: [NativeBindingTableColumnEntry]

    var orderedAXIndices: [UInt8] { columns.map(\.axIndex) }

    func matches(_ expected: NativeDeviceContextTableColumn, target: AXUIElement) -> Bool {
        expected.valid && ancestryIndex == expected.table_ancestry_index
            && orderedAXIndices == expected.ordered_ax_indices
            && columns.filter { CFEqual($0.header, target) }.count == 1
            && columns.contains { $0.axIndex == expected.target_ax_index && CFEqual($0.header, target) }
    }

    func hasSameReferences(as other: NativeBindingTableColumnGraph) -> Bool {
        guard ancestryIndex == other.ancestryIndex, columns.count == other.columns.count,
              CFEqual(table, other.table) else { return false }
        return zip(columns, other.columns).allSatisfy { pair in
            let (left, right) = pair
            return left.axIndex == right.axIndex
                && CFEqual(left.element, right.element)
                && CFEqual(left.header, right.header)
        }
    }
}

struct NativeBindingContextSnapshot {
    let fingerprint: NativeBindingFingerprint
    /// Ordered from target to the selected AX window/sheet.
    let path: [NativeBindingPathNode]
    /// Complete ordered AXChildren list for the target's actual parent.
    let targetSiblings: [NativeBindingSiblingNode]
    let window: AXUIElement
    let windowTitle: String?
    let sheetParent: AXUIElement?
    let tableColumnGraph: NativeBindingTableColumnGraph?

    /// Compares actual refs retained during two reads on this worker. Matching
    /// descriptors alone never satisfy this relationship check.
    func connectedPath() -> Bool {
        guard let root = path.last, CFEqual(window, root.element), root.parent == nil else { return false }
        for index in path.indices.dropLast() {
            guard let parent = path[index].parent,
                  CFEqual(parent, path[index + 1].element) else { return false }
        }
        return true
    }

    func sameWorkerGraph(as other: NativeBindingContextSnapshot) -> Bool {
        guard connectedPath(), other.connectedPath(),
              CFEqual(window, other.window),
              sameOptionalAXReference(sheetParent, other.sheetParent),
              sameOptionalTableColumnGraph(tableColumnGraph, other.tableColumnGraph),
              windowTitle == other.windowTitle,
              path.count == other.path.count,
              targetSiblings.count == other.targetSiblings.count else { return false }
        for (left, right) in zip(path, other.path) {
            guard CFEqual(left.element, right.element),
                  sameOptionalAXReference(left.parent, right.parent),
                  left.siblingIndex == right.siblingIndex,
                  left.role == right.role, left.subrole == right.subrole,
                  left.label == right.label, left.group == right.group,
                  left.frame == right.frame,
                  sameScrollContext(left.scroll, right.scroll) else { return false }
        }
        for (left, right) in zip(targetSiblings, other.targetSiblings) {
            guard CFEqual(left.element, right.element),
                  left.role == right.role, left.subrole == right.subrole,
                  left.label == right.label, left.frame == right.frame else { return false }
        }
        return true
    }
}

private func sameOptionalTableColumnGraph(_ lhs: NativeBindingTableColumnGraph?,
                                          _ rhs: NativeBindingTableColumnGraph?) -> Bool {
    switch (lhs, rhs) {
    case (nil, nil): return true
    case let (left?, right?): return left.hasSameReferences(as: right)
    default: return false
    }
}

private func sameOptionalAXReference(_ lhs: AXUIElement?, _ rhs: AXUIElement?) -> Bool {
    switch (lhs, rhs) {
    case (nil, nil): return true
    case let (left?, right?): return CFEqual(left, right)
    default: return false
    }
}

private func sameScrollAxis(_ lhs: NativeBindingScrollAxisContext?,
                            _ rhs: NativeBindingScrollAxisContext?) -> Bool {
    switch (lhs, rhs) {
    case (nil, nil): return true
    case let (left?, right?):
        return CFEqual(left.element, right.element) && left.value == right.value
            && left.minimum == right.minimum && left.maximum == right.maximum
    default: return false
    }
}

private func sameScrollContext(_ lhs: NativeBindingScrollContext?,
                               _ rhs: NativeBindingScrollContext?) -> Bool {
    switch (lhs, rhs) {
    case (nil, nil): return true
    case let (left?, right?):
        return sameScrollAxis(left.horizontal, right.horizontal)
            && sameScrollAxis(left.vertical, right.vertical)
    default: return false
    }
}

func menuPathRequiresTopLevel(_ role: String) -> Bool {
    role == kAXMenuRole || role == kAXMenuItemRole
}

/// A bounded parent path and child-reference lists only, never a second source
/// traversal. Every ref is created/read/destroyed on the hit-test worker.
final class NativeBindingReader {
    let allowedRoles: Set<String>
    let pid: Int32, bundle: String, windowID: UInt32, window: AXUIElement, deadline: UInt64
    let sheetParent: AXUIElement?
    let expectedTableColumn: NativeDeviceContextTableColumn?
    init(pid: Int32, bundle: String, windowID: UInt32, window: AXUIElement, deadline: UInt64,
         allowedRoles: Set<String> = HitRequest.allowedRoles, sheetParent: AXUIElement? = nil,
         tableColumn: NativeDeviceContextTableColumn? = nil) {
        self.pid = pid; self.bundle = bundle; self.windowID = windowID; self.window = window; self.deadline = deadline; self.allowedRoles = allowedRoles
        self.sheetParent = sheetParent
        self.expectedTableColumn = tableColumn
    }
    private enum ReadFailure: Error { case unavailable(String) }
    private(set) var failureReason = "binding path inconsistent"
    private func sameProcess(_ element: AXUIElement) throws {
        var owner: pid_t = 0
        let now = monotonicMS()
        guard now < deadline,
              AXUIElementSetMessagingTimeout(element, Float(min(25, deadline - now)) / 1000) == .success,
              AXUIElementGetPid(element, &owner) == .success, owner == pid else {
            throw ReadFailure.unavailable("path process or time unavailable")
        }
    }
    private func attribute(_ element: AXUIElement, _ key: String, optional: Bool = false) throws -> CFTypeRef? {
        try sameProcess(element)
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, key as CFString, &value)
        guard monotonicMS() < deadline else { throw ReadFailure.unavailable("binding time budget exceeded") }
        if optional && (error == .attributeUnsupported || error == .noValue) { return nil }
        guard error == .success, let value else { throw ReadFailure.unavailable("attribute \(key) unavailable (\(error.rawValue))") }
        return value
    }
    private func text(_ element: AXUIElement, _ key: String, optional: Bool = false) throws -> String? {
        guard let value = try attribute(element, key, optional: optional) else { return nil }
        guard let text = value as? String, text.utf8.count <= 512 else { throw ReadFailure.unavailable("invalid text type or budget") }
        return text
    }
    private func element(_ element: AXUIElement, _ key: String) throws -> AXUIElement {
        guard let value = try attribute(element, key), CFGetTypeID(value) == AXUIElementGetTypeID() else {
            throw ReadFailure.unavailable("invalid AX reference type for \(key)")
        }
        return value as! AXUIElement
    }
    private func actualTableColumns(target: AXUIElement,
                                    rootFirstPath: [NativeBindingPathNode],
                                    expected: NativeDeviceContextTableColumn) throws -> NativeBindingTableColumnGraph {
        guard expected.valid, rootFirstPath.count >= 2,
              Int(expected.table_ancestry_index) < rootFirstPath.count - 1 else {
            throw ReadFailure.unavailable("table ancestry index unavailable")
        }
        let tableAncestors = rootFirstPath.dropLast().enumerated().filter { $0.element.role == kAXTableRole }
        guard tableAncestors.count == 1,
              tableAncestors[0].offset == Int(expected.table_ancestry_index) else {
            throw ReadFailure.unavailable("AXTable ancestry is missing or ambiguous")
        }
        let table = tableAncestors[0].element.element
        try sameProcess(table)
        guard try text(table, kAXRoleAttribute) == kAXTableRole,
              CFEqual(try element(table, kAXWindowAttribute), window) else {
            throw ReadFailure.unavailable("AXTable owner window changed")
        }
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(table, kAXColumnsAttribute as CFString, &count)
        guard countError == .success, count > 0, count <= 64,
              count == expected.ordered_ax_indices.count, monotonicMS() < deadline else {
            throw ReadFailure.unavailable("AXColumns count unavailable or changed")
        }
        var rawColumns: CFArray?
        guard AXUIElementCopyAttributeValues(table, kAXColumnsAttribute as CFString,
                0, count, &rawColumns) == .success,
              let values = rawColumns as? [AnyObject], values.count == count,
              monotonicMS() < deadline else {
            throw ReadFailure.unavailable("AXColumns list unavailable or changed")
        }

        var seenIndices = Set<UInt8>()
        var seenColumns: [AXUIElement] = []
        var seenHeaders: [AXUIElement] = []
        var columns: [NativeBindingTableColumnEntry] = []
        columns.reserveCapacity(count)
        for (position, rawColumn) in values.enumerated() {
            guard CFGetTypeID(rawColumn) == AXUIElementGetTypeID() else {
                throw ReadFailure.unavailable("AXColumns contains a non-element")
            }
            let column = rawColumn as! AXUIElement
            guard !seenColumns.contains(where: { CFEqual($0, column) }) else {
                throw ReadFailure.unavailable("AXColumns contains a duplicate element")
            }
            seenColumns.append(column)
            try sameProcess(column)
            guard try text(column, kAXRoleAttribute) == kAXColumnRole,
                  CFEqual(try element(column, kAXParentAttribute), table),
                  CFEqual(try element(column, kAXWindowAttribute), window) else {
                throw ReadFailure.unavailable("AXColumn parent, role, or window changed")
            }
            guard let rawIndex = try attribute(column, kAXIndexAttribute),
                  CFGetTypeID(rawIndex) == CFNumberGetTypeID(),
                  let number = rawIndex as? NSNumber,
                  number.doubleValue.isFinite,
                  number.doubleValue.rounded(.towardZero) == number.doubleValue,
                  number.doubleValue >= 0, number.doubleValue < 64 else {
                throw ReadFailure.unavailable("AXColumn AXIndex is missing or invalid")
            }
            let index = UInt8(number.intValue)
            guard seenIndices.insert(index).inserted,
                  index == expected.ordered_ax_indices[position] else {
                throw ReadFailure.unavailable("AXColumn order or unique AXIndex changed")
            }
            let header = try element(column, kAXHeaderAttribute)
            try sameProcess(header)
            guard CFEqual(try element(header, kAXWindowAttribute), window),
                  !seenHeaders.contains(where: { CFEqual($0, header) }) else {
                throw ReadFailure.unavailable("AXHeader window changed or relation is ambiguous")
            }
            seenHeaders.append(header)
            columns.append(NativeBindingTableColumnEntry(element: column, axIndex: index, header: header))
        }
        guard seenIndices.count == count,
              expected.ordered_ax_indices.filter({ $0 == expected.target_ax_index }).count == 1,
              columns.filter({ CFEqual($0.header, target) }).count == 1,
              columns.contains(where: { $0.axIndex == expected.target_ax_index && CFEqual($0.header, target) }),
              monotonicMS() < deadline else {
            throw ReadFailure.unavailable("AXHeader does not uniquely identify the bound sortable column")
        }
        return NativeBindingTableColumnGraph(table: table,
            ancestryIndex: expected.table_ancestry_index, columns: columns)
    }
    private struct SiblingList {
        let elements: [AXUIElement]
        let index: UInt32
    }

    private func siblingList(_ child: AXUIElement, parent: AXUIElement) throws -> SiblingList {
        try sameProcess(parent)
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(parent, kAXChildrenAttribute as CFString, &count)
        guard countError == .success, count > 0, count <= 256, monotonicMS() < deadline else {
            throw ReadFailure.unavailable("child count unavailable (\(countError.rawValue), \(count))")
        }
        var array: CFArray?
        guard AXUIElementCopyAttributeValues(parent, kAXChildrenAttribute as CFString, 0, count, &array) == .success,
              let values = array as? [AnyObject], values.count == count, monotonicMS() < deadline else { throw ReadFailure.unavailable("child list unavailable") }
        var index: UInt32?
        for (i, value) in values.enumerated() {
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else { throw ReadFailure.unavailable("invalid child reference type") }
            if CFEqual(value, child) {
                guard index == nil else { throw ReadFailure.unavailable("duplicate child membership") }
                index = UInt32(i)
            }
        }
        guard let index else { throw ReadFailure.unavailable("target absent from parent children") }
        return SiblingList(elements: values.map { $0 as! AXUIElement }, index: index)
    }
    private func geometry(_ element: AXUIElement) throws -> HitRect {
        guard let p = try attribute(element, kAXPositionAttribute), CFGetTypeID(p) == AXValueGetTypeID(),
              let s = try attribute(element, kAXSizeAttribute), CFGetTypeID(s) == AXValueGetTypeID() else {
            throw ReadFailure.unavailable("geometry unavailable")
        }
        var point = CGPoint.zero, size = CGSize.zero
        guard AXValueGetValue(p as! AXValue, .cgPoint, &point), AXValueGetValue(s as! AXValue, .cgSize, &size) else {
            throw ReadFailure.unavailable("geometry unavailable")
        }
        let frame = HitRect(CGRect(origin: point, size: size))
        guard frame.valid else { throw ReadFailure.unavailable("geometry invalid") }; return frame
    }

    private func scrollAxis(_ element: AXUIElement, attribute name: String) throws -> NativeBindingScrollAxisContext? {
        guard let rawBar = try attribute(element, name, optional: true) else { return nil }
        guard CFGetTypeID(rawBar) == AXUIElementGetTypeID() else {
            throw ReadFailure.unavailable("scrollbar reference type is invalid")
        }
        let bar = rawBar as! AXUIElement
        try sameProcess(bar)
        guard try text(bar, kAXRoleAttribute) == kAXScrollBarRole else {
            throw ReadFailure.unavailable("scrollbar role changed")
        }
        guard let rawValue = try attribute(bar, kAXValueAttribute, optional: true) else { return nil }
        guard CFGetTypeID(rawValue) == CFNumberGetTypeID(),
              let value = rawValue as? NSNumber else {
            throw ReadFailure.unavailable("scrollbar value type is invalid")
        }

        let rawMinimum = try attribute(bar, kAXMinValueAttribute, optional: true)
        let rawMaximum = try attribute(bar, kAXMaxValueAttribute, optional: true)
        let minimum: Double?
        if let rawMinimum {
            guard CFGetTypeID(rawMinimum) == CFNumberGetTypeID(),
                  let number = rawMinimum as? NSNumber else {
                throw ReadFailure.unavailable("scrollbar minimum type is invalid")
            }
            minimum = number.doubleValue
        } else {
            minimum = nil
        }
        let maximum: Double?
        if let rawMaximum {
            guard CFGetTypeID(rawMaximum) == CFNumberGetTypeID(),
                  let number = rawMaximum as? NSNumber else {
                throw ReadFailure.unavailable("scrollbar maximum type is invalid")
            }
            maximum = number.doubleValue
        } else {
            maximum = nil
        }
        let position = value.doubleValue
            guard position.isFinite,
              minimum.map({ $0.isFinite && position >= $0 }) ?? true,
              maximum.map({ $0.isFinite && position <= $0 }) ?? true else { return nil }
        if let minimum, let maximum, minimum >= maximum { return nil }
        return NativeBindingScrollAxisContext(element: bar, value: position,
            minimum: minimum, maximum: maximum)
    }

    private func scrollContext(_ element: AXUIElement, role: String) throws -> NativeBindingScrollContext? {
        guard role == kAXScrollAreaRole else { return nil }
        let horizontal = try scrollAxis(element, attribute: kAXHorizontalScrollBarAttribute)
        let vertical = try scrollAxis(element, attribute: kAXVerticalScrollBarAttribute)
        guard horizontal != nil || vertical != nil else { return nil }
        return NativeBindingScrollContext(horizontal: horizontal, vertical: vertical)
    }

    private func isGroupRole(_ role: String) -> Bool {
        role == kAXGroupRole || role.hasSuffix("Group")
    }

    private struct BindingReadResult {
        let fingerprint: NativeBindingFingerprint
        let context: NativeBindingContextSnapshot?
    }

    func read(_ hit: AXUIElement) -> NativeBindingFingerprint? {
        readBinding(hit, menuProfile: false, captureContext: false)?.fingerprint
    }

    func readContext(_ hit: AXUIElement) -> NativeBindingContextSnapshot? {
        readBinding(hit, menuProfile: false, captureContext: true)?.context
    }

    func readMenuContext(_ hit: AXUIElement) -> NativeBindingContextSnapshot? {
        readBinding(hit, menuProfile: true, captureContext: true)?.context
    }

    func readMenuElement(_ hit: AXUIElement) -> NativeBindingFingerprint? {
        readBinding(hit, menuProfile: true, captureContext: false)?.fingerprint
    }

    private func readBinding(_ hit: AXUIElement, menuProfile: Bool,
                             captureContext: Bool) -> BindingReadResult? {
        do {
            let role = try text(hit, kAXRoleAttribute)!
            guard allowedRoles.contains(role) else { return nil }
            if CFEqual(hit, window) {
                guard role == kAXWindowRole || role == kAXSheetRole else { return nil }
            } else if menuProfile {
                guard [kAXMenuRole, kAXMenuItemRole].contains(role), CFEqual(try element(hit, kAXTopLevelUIElementAttribute), window) else { return nil }
                try sameProcess(hit)
                var value: CFTypeRef?
                let error = AXUIElementCopyAttributeValue(hit, kAXWindowAttribute as CFString, &value)
                guard monotonicMS() < deadline else { return nil }
                let sameRoot = value.map { CFGetTypeID($0) == AXUIElementGetTypeID() && CFEqual($0, window) } ?? false
                guard menuWindowAttributeAllowed(error: error, sameRoot: sameRoot) else { return nil }
            } else if let sheetParent {
                guard bundle == "com.lingxi.computer-user.mvp-fixture",
                      try text(window, kAXRoleAttribute) == kAXSheetRole,
                      try text(sheetParent, kAXRoleAttribute) == kAXWindowRole,
                      CFEqual(try element(hit, kAXTopLevelUIElementAttribute), window),
                      CFEqual(try element(hit, kAXWindowAttribute), sheetParent) else { return nil }
            } else {
                guard CFEqual(try element(hit, kAXWindowAttribute), window) else { return nil }
            }
            let subrole = try text(hit, kAXSubroleAttribute, optional: true)
            let hitTitle = try text(hit, kAXTitleAttribute, optional: true)
            let hitDescription = try text(hit, kAXDescriptionAttribute, optional: true)
            let label = hitTitle ?? hitDescription ?? ""
            let frame = try geometry(hit)
            var cursor = hit, seen = [hit], roles: [String] = [], group: String?, sibling: UInt32?
            var reversedPath: [NativeBindingPathNode] = []
            var targetSiblings: [NativeBindingSiblingNode] = []
            var windowTitle: String?
            while true {
                guard roles.count <= 32 else { return nil }
                let currentRole = try text(cursor, kAXRoleAttribute)!
                if menuProfile, menuPathRequiresTopLevel(currentRole),
                   !CFEqual(try element(cursor, kAXTopLevelUIElementAttribute), window) { return nil }
                let currentSubrole = try text(cursor, kAXSubroleAttribute, optional: true)
                if CFEqual(cursor, window) {
                    guard currentRole == kAXWindowRole || currentRole == kAXSheetRole || menuProfile else {
                        return nil
                    }
                    if captureContext {
                        let title = try text(cursor, kAXTitleAttribute, optional: true)
                        let description = try text(cursor, kAXDescriptionAttribute, optional: true)
                        windowTitle = title.map(bindingLabel)
                        let currentLabel = bindingLabel(title ?? description ?? "")
                        if CFEqual(cursor, hit) {
                            sibling = 0
                            targetSiblings.append(NativeBindingSiblingNode(element: cursor,
                                role: currentRole, subrole: currentSubrole,
                                label: currentLabel, frame: frame))
                        }
                        reversedPath.append(NativeBindingPathNode(element: cursor, parent: nil,
                            siblingIndex: nil, role: currentRole, subrole: currentSubrole,
                            label: currentLabel, group: nil, frame: try geometry(cursor),
                            scroll: try scrollContext(cursor, role: currentRole)))
                    }
                    break
                }

                guard roles.count < 32 else { return nil }
                let parent = try element(cursor, kAXParentAttribute)
                guard !seen.contains(where: { CFEqual($0, parent) }) else { return nil }
                seen.append(parent)
                let parentRole = try text(parent, kAXRoleAttribute)!
                let siblings: SiblingList
                do { siblings = try siblingList(cursor, parent: parent) }
                catch ReadFailure.unavailable(let reason) {
                    throw ReadFailure.unavailable("\(reason); ancestor \(roles.count) \(parentRole)")
                }
                if sibling == nil { sibling = siblings.index }
                if captureContext && reversedPath.isEmpty {
                    targetSiblings.reserveCapacity(siblings.elements.count)
                    for siblingElement in siblings.elements {
                        guard let siblingRole = try text(siblingElement, kAXRoleAttribute) else { return nil }
                        let siblingSubrole = try text(siblingElement, kAXSubroleAttribute, optional: true)
                        let siblingTitle = try text(siblingElement, kAXTitleAttribute, optional: true)
                        let siblingDescription = try text(siblingElement, kAXDescriptionAttribute, optional: true)
                        targetSiblings.append(NativeBindingSiblingNode(element: siblingElement,
                            role: siblingRole, subrole: siblingSubrole,
                            label: bindingLabel(siblingTitle ?? siblingDescription ?? ""),
                            frame: try geometry(siblingElement)))
                    }
                }
                if captureContext {
                    let currentLabel: String
                    if CFEqual(cursor, hit) {
                        currentLabel = bindingLabel(label)
                    } else {
                        let currentTitle = try text(cursor, kAXTitleAttribute, optional: true)
                        let currentDescription = try text(cursor, kAXDescriptionAttribute, optional: true)
                        currentLabel = bindingLabel(currentTitle ?? currentDescription ?? "")
                    }
                    let currentFrame = CFEqual(cursor, hit) ? frame : try geometry(cursor)
                    reversedPath.append(NativeBindingPathNode(element: cursor, parent: parent,
                        siblingIndex: siblings.index, role: currentRole, subrole: currentSubrole,
                        label: currentLabel, group: nil, frame: currentFrame,
                        scroll: try scrollContext(cursor, role: currentRole)))
                }
                roles.append(parentRole)
                if group == nil && isGroupRole(parentRole) {
                    let groupTitle = try text(parent, kAXTitleAttribute, optional: true)
                    let groupDescription = try text(parent, kAXDescriptionAttribute, optional: true)
                    let groupLabel = groupTitle ?? groupDescription
                    if let groupLabel, !bindingLabel(groupLabel).isEmpty {
                        group = groupLabel
                    }
                }
                cursor = parent
                if parentRole == kAXWindowRole && !CFEqual(parent, window) { return nil }
            }
            guard sibling != nil, monotonicMS() < deadline else { return nil }

            let computedGroup: String?
            let tableColumnGraph: NativeBindingTableColumnGraph?
            let context: NativeBindingContextSnapshot?
            if captureContext {
                guard targetSiblings.count > 0, !reversedPath.isEmpty,
                      CFEqual(reversedPath.last!.element, window) else { return nil }
                var inheritedGroup: String?
                var rootFirst: [NativeBindingPathNode] = []
                rootFirst.reserveCapacity(reversedPath.count)
                for node in reversedPath.reversed() {
                    rootFirst.append(NativeBindingPathNode(element: node.element, parent: node.parent,
                        siblingIndex: node.siblingIndex, role: node.role, subrole: node.subrole,
                        label: node.label, group: inheritedGroup, frame: node.frame, scroll: node.scroll))
                    if isGroupRole(node.role), !node.label.isEmpty { inheritedGroup = node.label }
                }
                reversedPath = Array(rootFirst.reversed())
                computedGroup = reversedPath.first?.group
                guard computedGroup.map(bindingLabel) == group.map(bindingLabel) else { return nil }
                tableColumnGraph = try expectedTableColumn.map {
                    try actualTableColumns(target: hit, rootFirstPath: rootFirst, expected: $0)
                }
            } else {
                computedGroup = group.map(bindingLabel)
                tableColumnGraph = nil
            }
            let proof = NativeBindingFingerprint(app: bundle, app_pid: UInt32(pid), window_id: "window-\(windowID)",
                ancestor_roles: roles.reversed(), group: group.map(bindingLabel), role: role, subrole: subrole,
                label: bindingLabel(label), sibling_index: sibling!, frame: frame.rounded, scroll_container: nil)
            guard proof.valid, monotonicMS() < deadline else { return nil }
            if captureContext, computedGroup != proof.group { return nil }
            if captureContext {
                context = NativeBindingContextSnapshot(fingerprint: proof, path: reversedPath,
                    targetSiblings: targetSiblings, window: window,
                    windowTitle: windowTitle, sheetParent: sheetParent,
                    tableColumnGraph: tableColumnGraph)
                for index in reversedPath.indices.dropLast() {
                    let node = reversedPath[index]
                    guard let expectedParent = node.parent,
                          CFEqual(try element(node.element, kAXParentAttribute), expectedParent),
                          CFEqual(expectedParent, reversedPath[index + 1].element) else { return nil }
                }
                if let targetNode = reversedPath.first, let targetParent = targetNode.parent {
                    let verifiedSiblings = try siblingList(targetNode.element, parent: targetParent)
                    guard verifiedSiblings.index == targetNode.siblingIndex,
                          verifiedSiblings.elements.count == targetSiblings.count,
                          zip(verifiedSiblings.elements, targetSiblings).allSatisfy({ CFEqual($0.0, $0.1.element) }) else { return nil }
                }
            } else {
                context = nil
            }
            return BindingReadResult(fingerprint: proof, context: context)
        } catch ReadFailure.unavailable(let reason) { failureReason = reason; return nil }
        catch { failureReason = "binding path unavailable"; return nil }
    }
}
