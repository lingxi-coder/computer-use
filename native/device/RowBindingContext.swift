import AppKit
import ApplicationServices
import CoreFoundation
import Foundation

/// A value-only selector condition. These values stay on the device worker and
/// are never written to diagnostics or serialized with AX references.
enum NativeRowSelectorValue: Codable, Sendable, Equatable {
    case text(String)
    case boolean(Bool)
    case number(Double)

    var valid: Bool {
        switch self {
        case .text(let value):
            !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                && value.utf8.count <= 2048
                && !value.unicodeScalars.contains { $0.properties.generalCategory == .control }
        case .boolean: true
        case .number(let value): value.isFinite && abs(value) <= 1.0e12
        }
    }

    static func fromAXValue(_ raw: CFTypeRef) -> NativeRowSelectorValue? {
        let type = CFGetTypeID(raw)
        if type == CFBooleanGetTypeID(), let value = raw as? NSNumber {
            return .boolean(value.boolValue)
        }
        if type == CFStringGetTypeID(), let value = raw as? String, value.utf8.count <= 2048 {
            return .text(value)
        }
        if type == CFNumberGetTypeID(), let value = raw as? NSNumber,
           value.doubleValue.isFinite {
            return .number(value.doubleValue)
        }
        return nil
    }

    private enum CodingKeys: String, CodingKey { case type, value }
    private enum ValueKind: String, Codable { case bool, text, number }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(ValueKind.self, forKey: .type) {
        case .bool:
            self = .boolean(try values.decode(Bool.self, forKey: .value))
        case .text:
            self = .text(try values.decode(String.self, forKey: .value))
        case .number:
            self = .number(try values.decode(Double.self, forKey: .value))
        }
    }

    func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .boolean(let value):
            try values.encode(ValueKind.bool, forKey: .type)
            try values.encode(value, forKey: .value)
        case .text(let value):
            try values.encode(ValueKind.text, forKey: .type)
            try values.encode(value, forKey: .value)
        case .number(let value):
            try values.encode(ValueKind.number, forKey: .type)
            try values.encode(value, forKey: .value)
        }
    }
}

struct NativeRowSelectorCondition: Equatable {
    let columnAXIndex: UInt8
    let headerLabel: String
    let value: NativeRowSelectorValue

    var valid: Bool {
        Int(columnAXIndex) < NativeRowBindingValidation.maximumColumns
            && !headerLabel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && headerLabel.utf8.count <= 256 && value.valid
    }
}

/// Pure parsing and budget rules shared by the worker and named self-tests.
/// They validate public AX values, not synthetic AX object identity.
enum NativeRowBindingValidation {
    static let maximumVisibleRows = 256
    static let maximumColumns = 64
    static let maximumCellsPerRow = 128
    static let maximumTotalCells = 2_048
    static let maximumSelectorConditions = 32
    static let maximumValueChildren = 8
    static let maximumAXCallTimeoutMS: UInt64 = 25
    static let maximumBatchAttributes = 8

    static func rowAXIndex(_ raw: CFTypeRef?) -> UInt32? {
        guard let raw, CFGetTypeID(raw) == CFNumberGetTypeID(),
              let number = raw as? NSNumber else { return nil }
        let value = number.doubleValue
        guard value.isFinite, value.rounded(.towardZero) == value,
              value >= 0, value <= Double(UInt32.max) else { return nil }
        return UInt32(value)
    }

    static func axEnabledObservation(error: AXError, value: CFTypeRef?, withinDeadline: Bool)
        -> NativeAXEnabledObservation {
        guard withinDeadline else { return .unavailable }
        if error == .attributeUnsupported { return .attributeUnsupported }
        guard error == .success, let value,
              CFGetTypeID(value) == CFBooleanGetTypeID(),
              let boolean = value as? NSNumber else { return .unavailable }
        return boolean.boolValue ? .enabled : .disabled
    }

    static func rowEnabledStatesMatch(table: NativeAXEnabledObservation,
                                      target: NativeAXEnabledObservation,
                                      expectedTable: NativeAXEnabledObservation,
                                      expectedTarget: NativeAXEnabledObservation) -> Bool {
        table == expectedTable && target == expectedTarget
            && table.permitsRowSelection && target.permitsRowSelection
    }

    static func singletonRowIndexRange(location: Int, length: Int) -> UInt32? {
        guard length == 1 else { return nil }
        return UInt32(exactly: location)
    }

    static func singletonColumnIndexRange(location: Int, length: Int) -> UInt8? {
        guard length == 1, (0..<maximumColumns).contains(location) else { return nil }
        return UInt8(location)
    }

    static func validBoundedListCount(_ count: Int, limit: Int) -> Bool {
        count > 0 && count <= limit
    }

    static func rowCellListLimit(currentTotal: Int) -> Int? {
        guard currentTotal >= 0, currentTotal <= maximumTotalCells else { return nil }
        return min(maximumCellsPerRow, maximumTotalCells - currentTotal)
    }

    static func accumulatedCellCount(currentTotal: Int, rowCount: Int) -> Int? {
        guard currentTotal >= 0, currentTotal <= maximumTotalCells,
              rowCount > 0, rowCount <= maximumCellsPerRow,
              rowCount <= maximumTotalCells - currentTotal else { return nil }
        return currentTotal + rowCount
    }

    static func rowIndexRange(_ raw: CFTypeRef?) -> UInt32? {
        guard let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
        var range = CFRange()
        guard AXValueGetValue(unsafeDowncast(raw, to: AXValue.self), .cfRange, &range) else { return nil }
        return singletonRowIndexRange(location: range.location, length: range.length)
    }

    static func columnIndexRange(_ raw: CFTypeRef?) -> UInt8? {
        guard let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
        var range = CFRange()
        guard AXValueGetValue(unsafeDowncast(raw, to: AXValue.self), .cfRange, &range) else { return nil }
        return singletonColumnIndexRange(location: range.location, length: range.length)
    }

    static func visibleIndicesMatch(_ actual: [UInt32], target: UInt32,
                                    expected: [UInt32]?) -> Bool {
        guard !actual.isEmpty, actual.count <= maximumVisibleRows,
              Set(actual).count == actual.count, actual.contains(target) else { return false }
        guard let expected else { return true }
        return expected.count == actual.count && expected == actual
            && Set(expected).count == expected.count
    }

    static func visibleRowsMatch(_ actual: [(UInt32, HitRect)], target: UInt32,
                                 expected: [NativeDeviceContextVisibleRow]) -> Bool {
        guard !actual.isEmpty, actual.count <= maximumVisibleRows,
              actual.count == expected.count,
              Set(actual.map { $0.0 }).count == actual.count,
              Set(expected.map(\.ax_index)).count == expected.count else { return false }
        return zip(actual, expected).allSatisfy { pair in
            pair.0.0 == pair.1.ax_index && pair.0.1 == pair.1.frame
        } && actual.filter { $0.0 == target }.count == 1
    }

    static func orderedColumnsMatch(_ actual: [NativeRowColumnSnapshot],
                                    expected: [NativeDeviceContextRowColumn]) -> Bool {
        guard !actual.isEmpty, actual.count <= maximumColumns,
              actual.count == expected.count,
              Set(actual.map(\.axIndex)).count == actual.count,
              Set(expected.map(\.ax_index)).count == expected.count else { return false }
        return zip(actual, expected).allSatisfy { pair in
            pair.0.axIndex == pair.1.ax_index && pair.0.headerLabel == pair.1.header
                && pair.0.frame == pair.1.frame
        }
    }

    static func viewportFrameMatches(window: HitRect, scrollPath: [HitRect],
                                     expected: HitRect) -> Bool {
        guard window.valid, !scrollPath.isEmpty, scrollPath.allSatisfy(\.valid), expected.valid else {
            return false
        }
        var clipped = window.rect
        for frame in scrollPath {
            clipped = clipped.intersection(frame.rect)
            if clipped.isNull || clipped.isEmpty { return false }
        }
        return HitRect(clipped) == expected
    }

    static func uniqueSelectorMatch(_ matchingAXIndices: [UInt32], target: UInt32) -> Bool {
        matchingAXIndices.count == 1 && matchingAXIndices.first == target
    }

    static func validSelectorConditions(_ conditions: [NativeRowSelectorCondition]) -> Bool {
        guard !conditions.isEmpty, conditions.count <= maximumSelectorConditions,
              conditions.allSatisfy(\.valid) else { return false }
        return Set(conditions.map(\.columnAXIndex)).count == conditions.count
            && Set(conditions.map {
                $0.headerLabel.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            }).count == conditions.count
    }

    static func callTimeoutMilliseconds(now: UInt64, deadline: UInt64) -> UInt64? {
        guard now < deadline else { return nil }
        return min(maximumAXCallTimeoutMS, deadline - now)
    }

    static func decodeBatchResponse(error: AXError, values: CFArray?,
                                    attributes: [NativeRowBatchAttribute]) throws -> [NativeRowBatchSlot] {
        let names = attributes.map(\.name)
        guard !attributes.isEmpty, attributes.count <= maximumBatchAttributes,
              names.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 128 }),
              Set(names).count == names.count else {
            throw NativeRowBatchDecodeFailure.invalid("AX batch attribute request is invalid")
        }
        guard error == .success else {
            guard values == nil else {
                throw NativeRowBatchDecodeFailure.invalid(
                    "AX batch returned values with global AXError \(error.rawValue) for \(names.joined(separator: ","))")
            }
            throw NativeRowBatchDecodeFailure.invalid(
                "AX batch failed with AXError \(error.rawValue) for \(names.joined(separator: ","))")
        }
        guard let values, CFGetTypeID(values) == CFArrayGetTypeID(),
              CFArrayGetCount(values) == attributes.count else {
            throw NativeRowBatchDecodeFailure.invalid(
                "AX batch result shape differs for \(names.joined(separator: ","))")
        }
        let slots = values as [AnyObject]
        return try attributes.indices.map { index in
            try decodeBatchSlot(slots[index], attribute: attributes[index])
        }
    }

    private static func decodeBatchSlot(_ raw: AnyObject,
                                        attribute: NativeRowBatchAttribute) throws -> NativeRowBatchSlot {
        if CFGetTypeID(raw) == CFNullGetTypeID() {
            guard attribute.allowMissing else {
                throw NativeRowBatchDecodeFailure.invalid("AX attribute \(attribute.name) returned CFNull")
            }
            return .absent
        }
        if CFGetTypeID(raw) == AXValueGetTypeID() {
            let wrapped = unsafeDowncast(raw, to: AXValue.self)
            if AXValueGetType(wrapped) == .axError {
                var attributeError: AXError = .success
                guard AXValueGetValue(wrapped, .axError, &attributeError) else {
                    throw NativeRowBatchDecodeFailure.invalid("AX attribute \(attribute.name) has a malformed AXError value")
                }
                if attribute.allowMissing && (attributeError == .noValue || attributeError == .attributeUnsupported) {
                    return .absent
                }
                throw NativeRowBatchDecodeFailure.invalid(
                    "AX attribute \(attribute.name) returned AXError \(attributeError.rawValue)")
            }
        }
        return .value(raw)
    }
}

struct NativeRowBatchAttribute {
    let name: String
    let allowMissing: Bool

    static func required(_ name: String) -> NativeRowBatchAttribute {
        NativeRowBatchAttribute(name: name, allowMissing: false)
    }

    static func optional(_ name: String) -> NativeRowBatchAttribute {
        NativeRowBatchAttribute(name: name, allowMissing: true)
    }
}

enum NativeRowBatchSlot {
    case value(CFTypeRef)
    case absent
}

private enum NativeRowBatchDecodeFailure: Error {
    case invalid(String)
}

struct NativeRowConditionCellSnapshot {
    let conditionIndex: Int
    let element: AXUIElement
    let valueElement: AXUIElement?
    let rowAXIndex: UInt32
    let columnAXIndex: UInt8
    let value: NativeRowSelectorValue
}

struct NativeVisibleRowSnapshot {
    let element: AXUIElement
    let axIndex: UInt32
    let frame: HitRect
    let conditionCells: [NativeRowConditionCellSnapshot]
}

struct NativeRowColumnSnapshot {
    let element: AXUIElement
    let axIndex: UInt8
    let frame: HitRect
    let header: AXUIElement
    let headerLabel: String
}

struct NativeRowAncestorSnapshot {
    let element: AXUIElement
    let role: String
    let subrole: String?
    let rawTitle: String?
    let normalizedLabel: String
    let rawDescription: String?
    let frame: HitRect
    let modal: Bool?
}

/// Worker-local evidence from two bounded reads. It deliberately is not
/// Codable/Sendable and carries no cross-process AX identity claim.
struct NativeRowBindingSnapshot {
    let window: AXUIElement
    let table: AXUIElement
    let pointHitRow: AXUIElement
    let targetAXIndex: UInt32
    let windowFrame: HitRect
    let tableFrame: HitRect
    let viewportFrame: HitRect
    let targetRowFrame: HitRect
    let tableAXEnabled: NativeAXEnabledObservation
    let targetAXEnabled: NativeAXEnabledObservation
    let targetSelected: Bool
    let targetSelectedSettable: Bool
    let ancestorPath: [NativeRowAncestorSnapshot]
    let scrollPath: [NativeRowAncestorSnapshot]
    let visibleRows: [NativeVisibleRowSnapshot]
    let columns: [NativeRowColumnSnapshot]

    func sameWorkerGraph(as other: NativeRowBindingSnapshot) -> Bool {
        guard CFEqual(window, other.window), CFEqual(table, other.table),
              CFEqual(pointHitRow, other.pointHitRow),
              targetAXIndex == other.targetAXIndex,
              windowFrame == other.windowFrame,
              tableFrame == other.tableFrame,
              viewportFrame == other.viewportFrame,
              targetRowFrame == other.targetRowFrame,
              NativeRowBindingValidation.rowEnabledStatesMatch(table: tableAXEnabled,
                  target: targetAXEnabled, expectedTable: other.tableAXEnabled,
                  expectedTarget: other.targetAXEnabled),
              targetSelected == other.targetSelected,
              targetSelectedSettable == other.targetSelectedSettable,
              ancestorPath.count == other.ancestorPath.count,
              scrollPath.count == other.scrollPath.count,
              visibleRows.count == other.visibleRows.count,
              columns.count == other.columns.count else { return false }
        for (left, right) in zip(ancestorPath, other.ancestorPath) {
            guard CFEqual(left.element, right.element), left.role == right.role,
                  left.subrole == right.subrole, left.rawTitle == right.rawTitle,
                  left.normalizedLabel == right.normalizedLabel,
                  left.rawDescription == right.rawDescription, left.frame == right.frame,
                  left.modal == right.modal else { return false }
        }
        for (left, right) in zip(scrollPath, other.scrollPath) {
            guard CFEqual(left.element, right.element), left.rawDescription == right.rawDescription,
                  left.frame == right.frame else { return false }
        }
        for (left, right) in zip(visibleRows, other.visibleRows) {
            guard CFEqual(left.element, right.element), left.axIndex == right.axIndex,
                  left.frame == right.frame,
                  left.conditionCells.count == right.conditionCells.count else { return false }
            for (leftCell, rightCell) in zip(left.conditionCells, right.conditionCells) {
                guard leftCell.conditionIndex == rightCell.conditionIndex,
                      CFEqual(leftCell.element, rightCell.element),
                      sameNativeRowAXReference(leftCell.valueElement, rightCell.valueElement),
                      leftCell.rowAXIndex == rightCell.rowAXIndex,
                      leftCell.columnAXIndex == rightCell.columnAXIndex,
                      leftCell.value == rightCell.value else { return false }
            }
        }
        for (left, right) in zip(columns, other.columns) {
            guard CFEqual(left.element, right.element), left.axIndex == right.axIndex,
                  left.frame == right.frame,
                  CFEqual(left.header, right.header), left.headerLabel == right.headerLabel else { return false }
        }
        return true
    }
}

private func sameNativeRowAXReference(_ left: AXUIElement?, _ right: AXUIElement?) -> Bool {
    switch (left, right) {
    case (nil, nil): true
    case (let left?, let right?): CFEqual(left, right)
    default: false
    }
}

private enum NativeRowBindingReadFailure: Error {
    case unavailable(String)
}

/// Reads AXEnabled from an element whose process/window relationship was
/// already checked by the caller. Every call is fresh and uses the original
/// row-operation deadline with the normal per-call cap.
func readNativeAXEnabledObservation(_ element: AXUIElement, deadline: UInt64)
    -> NativeAXEnabledObservation {
    guard let timeoutMS = NativeRowBindingValidation.callTimeoutMilliseconds(
        now: monotonicMS(), deadline: deadline),
          AXUIElementSetMessagingTimeout(element, Float(timeoutMS) / 1000) == .success,
          monotonicMS() < deadline else { return .unavailable }
    var raw: CFTypeRef?
    let error = AXUIElementCopyAttributeValue(element, kAXEnabledAttribute as CFString, &raw)
    return NativeRowBindingValidation.axEnabledObservation(error: error, value: raw,
        withinDeadline: monotonicMS() < deadline)
}

/// Reads only a supplied table's public AXVisibleRows/AXColumns and direct row
/// cells. The point-hit AXRow handle is never replaced by an ancestor or child.
/// The caller supplies its original deadline; this reader never extends it.
final class NativeRowBindingReader {
    private let pid: Int32
    private let bundle: String
    private let window: AXUIElement
    private let table: AXUIElement
    private let deadline: UInt64
    private let pointHitRow: AXUIElement
    private var verifiedProcessRefs: [AXUIElement] = []
    private(set) var failureReason = "row context unavailable"

    init(pid: Int32, bundle: String, selectedWindow: AXUIElement, table: AXUIElement,
         deadline: UInt64, pointHitRow: AXUIElement) {
        self.pid = pid
        self.bundle = bundle
        self.window = selectedWindow
        self.table = table
        self.deadline = deadline
        self.pointHitRow = pointHitRow
    }

    /// The witness carries values and AX indices only. Every AX proxy used by
    /// this comparison is acquired and retained locally on this worker.
    func read(context: NativeDeviceContextTableViewportV1,
              binding: NativeBindingFingerprint) -> NativeRowBindingSnapshot? {
        failureReason = "row context unavailable"
        guard pid > 0, !bundle.isEmpty, bundle.utf8.count <= 256,
              context.valid, binding.valid,
              context.app.bundle_id == bundle, context.app.pid == UInt32(pid),
              context.target.object.role == binding.role,
              context.target.object.subrole == binding.subrole,
              context.target.object.label == binding.label,
              context.target.group == binding.group,
              NativeRowBindingValidation.validSelectorConditions(context.selector_columns.map {
                  NativeRowSelectorCondition(columnAXIndex: $0.ax_index,
                      headerLabel: $0.header, value: $0.expected_value)
              }) else {
            failureReason = "row selector or scope is invalid"
            return nil
        }
        do {
            guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
                  app.bundleIdentifier == bundle else {
                throw NativeRowBindingReadFailure.unavailable("target process identity changed")
            }
            return try readCurrent(context: context, binding: binding)
        } catch NativeRowBindingReadFailure.unavailable(let reason) {
            failureReason = reason
            return nil
        } catch {
            failureReason = "row context could not be read"
            return nil
        }
    }

    private func readCurrent(context: NativeDeviceContextTableViewportV1,
                             binding: NativeBindingFingerprint) throws -> NativeRowBindingSnapshot {
        try prepare(window)
        try prepare(table)
        try prepare(pointHitRow)
        let (ancestorPath, scrollPath) = try readAncestorPath(context: context)
        let selectorConditions = context.selector_columns.map {
            NativeRowSelectorCondition(columnAXIndex: $0.ax_index,
                headerLabel: $0.header, value: $0.expected_value)
        }
        let targetAXIndex = context.target_row_ax_index
        let rowAttributes = try batch(pointHitRow, [
            .required(kAXRoleAttribute), .optional(kAXSubroleAttribute),
            .required(kAXParentAttribute), .required(kAXWindowAttribute),
            .required(kAXIndexAttribute), .required(kAXPositionAttribute), .required(kAXSizeAttribute)])
        let rowLabelAttributes = try batch(pointHitRow, [
            .optional(kAXTitleAttribute), .optional(kAXDescriptionAttribute)])
        let targetRowFrame = try frame(rowAttributes, positionIndex: 5, sizeIndex: 6)
        let rowTitle = try optionalText(rowLabelAttributes, 0, kAXTitleAttribute)
        let rowDescription = try optionalText(rowLabelAttributes, 1, kAXDescriptionAttribute)
        let targetLabel = bindingLabel(rowTitle ?? rowDescription ?? "")
        guard ancestorPath.map(\.role) == binding.ancestor_roles,
              ancestorPath.first.map({ CFEqual($0.element, window) }) == true,
              ancestorPath.last.map({ CFEqual($0.element, table) && $0.role == kAXTableRole }) == true,
              ancestorPath.filter({ $0.role == kAXTableRole }).count == 1,
              let actualWindow = ancestorPath.first,
              actualWindow.frame == context.window.frame,
              actualWindow.rawTitle.map(bindingLabel) == context.window.title,
              actualWindow.modal == context.window.modal,
              context.window.window_id == binding.window_id,
              context.window.owner_bundle_id == bundle,
              (context.window.kind == .window && actualWindow.role == kAXWindowRole
                  && !context.window.modal && actualWindow.subrole == kAXStandardWindowSubrole)
                  || (context.window.kind == .sheet && actualWindow.role == kAXSheetRole
                      && context.window.modal),
              let actualTable = ancestorPath.last,
              actualTable.frame == context.table.frame,
              actualTable.normalizedLabel == bindingLabel(context.table.label),
              scrollPath.count == context.scroll_path.count,
              zip(scrollPath, context.scroll_path).allSatisfy({ pair in
                  pair.0.role == pair.1.role && pair.0.rawDescription == pair.1.label
                      && pair.0.frame == pair.1.frame
              }),
              NativeRowBindingValidation.viewportFrameMatches(
                  window: actualWindow.frame, scrollPath: scrollPath.map(\.frame),
                  expected: context.viewport_frame),
              actualGroup(in: ancestorPath) == context.target.group,
              actualGroup(in: ancestorPath) == binding.group,
              try text(rowAttributes, 0, kAXRoleAttribute) == kAXRowRole,
              try optionalText(rowAttributes, 1, kAXSubroleAttribute) == "AXTableRow",
              CFEqual(try element(rowAttributes, 2, kAXParentAttribute), table),
              CFEqual(try element(rowAttributes, 3, kAXWindowAttribute), window),
              let hitRowIndex = NativeRowBindingValidation.rowAXIndex(
                  try value(rowAttributes, 4, kAXIndexAttribute)),
              hitRowIndex == targetAXIndex,
              targetRowFrame == context.target.object.frame,
              targetRowFrame == context.visible_rows.first(where: { $0.ax_index == targetAXIndex })?.frame,
              targetLabel == context.target.object.label else {
            throw NativeRowBindingReadFailure.unavailable("point-hit row, table, or window relation changed")
        }

        guard let visibleRows = try elements(table, kAXVisibleRowsAttribute,
                limit: NativeRowBindingValidation.maximumVisibleRows),
              let columns = try elements(table, kAXColumnsAttribute,
                limit: NativeRowBindingValidation.maximumColumns) else {
            throw NativeRowBindingReadFailure.unavailable("AXVisibleRows or AXColumns is unavailable or over budget")
        }
        guard !visibleRows.isEmpty, !columns.isEmpty else {
            throw NativeRowBindingReadFailure.unavailable("table has no bounded visible rows or columns")
        }

        var visibleFrames: [(UInt32, HitRect)] = []
        var visibleSnapshots: [NativeVisibleRowSnapshot] = []
        var pointHitOccurrences = 0
        var selectorMatchingIndices: [UInt32] = []
        var totalCellCount = 0
        visibleFrames.reserveCapacity(visibleRows.count)
        visibleSnapshots.reserveCapacity(visibleRows.count)

        let columnSnapshots = try readColumns(columns, expected: context.ordered_columns)
        let columnsByIndex = Dictionary(uniqueKeysWithValues: columnSnapshots.map { ($0.axIndex, $0) })
        for condition in selectorConditions {
            guard let column = columnsByIndex[condition.columnAXIndex],
                  column.headerLabel == condition.headerLabel else {
                throw NativeRowBindingReadFailure.unavailable("selector column header relation changed")
            }
        }

        guard visibleRows.count == context.visible_rows.count else {
            throw NativeRowBindingReadFailure.unavailable("AXVisibleRows count differs from the complete Source order")
        }
        for (rowPosition, row) in visibleRows.enumerated() {
            let expectedRow = context.visible_rows[rowPosition]
            let rowAttributes = try batch(row, [
                .required(kAXRoleAttribute), .required(kAXSubroleAttribute),
                .required(kAXParentAttribute), .required(kAXWindowAttribute),
                .required(kAXIndexAttribute), .required(kAXPositionAttribute),
                .required(kAXSizeAttribute)])
            let rowFrame = try frame(rowAttributes, positionIndex: 5, sizeIndex: 6)
            guard try text(rowAttributes, 0, kAXRoleAttribute) == kAXRowRole,
                  try text(rowAttributes, 1, kAXSubroleAttribute) == "AXTableRow",
                  CFEqual(try element(rowAttributes, 2, kAXParentAttribute), table),
                  CFEqual(try element(rowAttributes, 3, kAXWindowAttribute), window),
                  let rowIndex = NativeRowBindingValidation.rowAXIndex(try value(rowAttributes, 4, kAXIndexAttribute)),
                  rowIndex == expectedRow.ax_index,
                  rowFrame == expectedRow.frame else {
                throw NativeRowBindingReadFailure.unavailable("visible row identity or ownership is invalid")
            }
            if CFEqual(row, pointHitRow) { pointHitOccurrences += 1 }
            visibleFrames.append((rowIndex, rowFrame))

            guard let rowCellLimit = NativeRowBindingValidation.rowCellListLimit(
                    currentTotal: totalCellCount),
                  let cells = try elements(row, kAXChildrenAttribute, limit: rowCellLimit),
                  !cells.isEmpty,
                  let nextCellCount = NativeRowBindingValidation.accumulatedCellCount(
                    currentTotal: totalCellCount, rowCount: cells.count) else {
                throw NativeRowBindingReadFailure.unavailable("visible row cells are unavailable or incomplete")
            }
            totalCellCount = nextCellCount
            var cellsByColumn: [UInt8: [(AXUIElement, NativeRowSelectorValue?, AXUIElement?)]] = [:]
            for cell in cells {
                let cellAttributes = try batch(cell, [
                    .required(kAXRoleAttribute), .optional(kAXSubroleAttribute),
                    .required(kAXParentAttribute), .required(kAXWindowAttribute),
                    .required(kAXRowIndexRangeAttribute), .required(kAXColumnIndexRangeAttribute)])
                guard try text(cellAttributes, 0, kAXRoleAttribute) == kAXCellRole,
                      try optionalText(cellAttributes, 1, kAXSubroleAttribute) != "AXSecureTextField",
                      CFEqual(try element(cellAttributes, 2, kAXParentAttribute), row),
                      CFEqual(try element(cellAttributes, 3, kAXWindowAttribute), window),
                      let cellRowIndex = NativeRowBindingValidation.rowIndexRange(try value(cellAttributes, 4, kAXRowIndexRangeAttribute)),
                      cellRowIndex == rowIndex,
                      let columnIndex = NativeRowBindingValidation.columnIndexRange(try value(cellAttributes, 5, kAXColumnIndexRangeAttribute)),
                      columnsByIndex[columnIndex] != nil else {
                    throw NativeRowBindingReadFailure.unavailable("cell row/column range or parent relation is invalid")
                }
                let needsValue = selectorConditions.contains { $0.columnAXIndex == columnIndex }
                if needsValue {
                    let (value, valueElement) = try cellValue(cell)
                    cellsByColumn[columnIndex, default: []].append((cell, value, valueElement))
                } else {
                    cellsByColumn[columnIndex, default: []].append((cell, nil, nil))
                }
            }

            var conditionCells: [NativeRowConditionCellSnapshot] = []
            conditionCells.reserveCapacity(selectorConditions.count)
            var matchesSelector = true
            for (conditionIndex, condition) in selectorConditions.enumerated() {
                guard let candidates = cellsByColumn[condition.columnAXIndex], candidates.count == 1,
                      let value = candidates[0].1 else {
                    throw NativeRowBindingReadFailure.unavailable("selector cell is missing or ambiguous")
                }
                if value != condition.value { matchesSelector = false }
                guard value == expectedRow.selector_values[conditionIndex] else {
                    throw NativeRowBindingReadFailure.unavailable("visible row selector value changed from Source")
                }
                conditionCells.append(NativeRowConditionCellSnapshot(conditionIndex: conditionIndex,
                    element: candidates[0].0, valueElement: candidates[0].2,
                    rowAXIndex: rowIndex, columnAXIndex: condition.columnAXIndex, value: value))
            }
            visibleSnapshots.append(NativeVisibleRowSnapshot(element: row, axIndex: rowIndex,
                frame: rowFrame, conditionCells: conditionCells))
            if matchesSelector { selectorMatchingIndices.append(rowIndex) }
        }

        guard NativeRowBindingValidation.visibleRowsMatch(visibleFrames,
                  target: targetAXIndex, expected: context.visible_rows),
              pointHitOccurrences == 1,
              NativeRowBindingValidation.uniqueSelectorMatch(selectorMatchingIndices, target: targetAXIndex),
              let targetPosition = visibleSnapshots.firstIndex(where: { CFEqual($0.element, pointHitRow) }),
              visibleSnapshots[targetPosition].axIndex == targetAXIndex,
              visibleSnapshots[targetPosition].frame == targetRowFrame else {
            throw NativeRowBindingReadFailure.unavailable("point-hit row does not uniquely match the complete selector")
        }

        let tableAXEnabled = try readAXEnabled(table)
        let targetAXEnabled = try readAXEnabled(pointHitRow)
        guard NativeRowBindingValidation.rowEnabledStatesMatch(table: tableAXEnabled,
                  target: targetAXEnabled, expectedTable: context.table_ax_enabled,
                  expectedTarget: context.target_ax_enabled) else {
            throw NativeRowBindingReadFailure.unavailable("table or point-hit row AXEnabled state changed or is not actionable")
        }

        let selectedAttributes = try batch(pointHitRow, [.required(kAXSelectedAttribute)])
        let rawSelected = try value(selectedAttributes, 0, kAXSelectedAttribute)
        guard CFGetTypeID(rawSelected) == CFBooleanGetTypeID(),
              let selected = (rawSelected as? NSNumber)?.boolValue,
              selected == context.target_selected, !selected else {
            throw NativeRowBindingReadFailure.unavailable("target row selection state is not explicitly false")
        }
        try prepare(pointHitRow)
        var settable = DarwinBoolean(false)
        let settableError = AXUIElementIsAttributeSettable(pointHitRow, kAXSelectedAttribute as CFString, &settable)
        guard monotonicMS() < deadline, settableError == .success,
              settable.boolValue == context.target_ax_selected_settable,
              settable.boolValue else {
            throw NativeRowBindingReadFailure.unavailable("AXSelected is not explicitly settable for the hit row")
        }

        return NativeRowBindingSnapshot(window: window, table: table, pointHitRow: pointHitRow,
            targetAXIndex: targetAXIndex, windowFrame: actualWindow.frame,
            tableFrame: actualTable.frame, viewportFrame: context.viewport_frame,
            targetRowFrame: targetRowFrame, tableAXEnabled: tableAXEnabled,
            targetAXEnabled: targetAXEnabled, targetSelected: selected,
            targetSelectedSettable: settable.boolValue, ancestorPath: ancestorPath,
            scrollPath: scrollPath,
            visibleRows: visibleSnapshots, columns: columnSnapshots)
    }

    private func readAncestorPath(context: NativeDeviceContextTableViewportV1)
        throws -> ([NativeRowAncestorSnapshot], [NativeRowAncestorSnapshot]) {
        var reversed: [NativeRowAncestorSnapshot] = []
        reversed.reserveCapacity(8)
        var current = table
        var reachedWindow = false

        for _ in 0..<32 {
            if CFEqual(current, window) {
                let attributes = try batch(window, [
                    .required(kAXRoleAttribute), .optional(kAXSubroleAttribute),
                    .optional(kAXTitleAttribute), .optional(kAXDescriptionAttribute),
                    .required(kAXPositionAttribute), .required(kAXSizeAttribute),
                    .required(kAXModalAttribute)])
                let role = try text(attributes, 0, kAXRoleAttribute)
                let subrole = try optionalText(attributes, 1, kAXSubroleAttribute)
                let title = try optionalText(attributes, 2, kAXTitleAttribute)
                let description = try optionalText(attributes, 3, kAXDescriptionAttribute)
                let frame = try frame(attributes, positionIndex: 4, sizeIndex: 5)
                let rawModal = try value(attributes, 6, kAXModalAttribute)
                guard CFGetTypeID(rawModal) == CFBooleanGetTypeID(),
                      let modal = (rawModal as? NSNumber)?.boolValue else {
                    throw NativeRowBindingReadFailure.unavailable("selected window modal state is not a boolean")
                }
                reversed.append(NativeRowAncestorSnapshot(element: window, role: role,
                    subrole: subrole, rawTitle: title,
                    normalizedLabel: bindingLabel(title ?? description ?? ""),
                    rawDescription: description, frame: frame, modal: modal))
                reachedWindow = true
                break
            }

            let attributes = try batch(current, [
                .required(kAXRoleAttribute), .optional(kAXSubroleAttribute),
                .optional(kAXTitleAttribute), .optional(kAXDescriptionAttribute),
                .required(kAXParentAttribute), .required(kAXWindowAttribute),
                .required(kAXPositionAttribute), .required(kAXSizeAttribute)])
            let role = try text(attributes, 0, kAXRoleAttribute)
            let subrole = try optionalText(attributes, 1, kAXSubroleAttribute)
            let title = try optionalText(attributes, 2, kAXTitleAttribute)
            let description = try optionalText(attributes, 3, kAXDescriptionAttribute)
            let parent = try element(attributes, 4, kAXParentAttribute)
            let ownerWindow = try element(attributes, 5, kAXWindowAttribute)
            let frame = try frame(attributes, positionIndex: 6, sizeIndex: 7)
            guard CFEqual(ownerWindow, window) else {
                throw NativeRowBindingReadFailure.unavailable("ancestor AXWindow differs from selected window")
            }
            reversed.append(NativeRowAncestorSnapshot(element: current, role: role,
                subrole: subrole, rawTitle: title,
                normalizedLabel: bindingLabel(title ?? description ?? ""),
                rawDescription: description, frame: frame, modal: nil))
            current = parent
        }

        guard reachedWindow, !reversed.isEmpty, reversed.count <= 32,
              reversed.last.map({ CFEqual($0.element, window) }) == true else {
            throw NativeRowBindingReadFailure.unavailable("table parent path does not reach selected window")
        }
        let path = Array(reversed.reversed())
        guard path.last.map({ CFEqual($0.element, table) && $0.role == kAXTableRole }) == true,
              path.filter({ $0.role == kAXTableRole }).count == 1 else {
            throw NativeRowBindingReadFailure.unavailable("table parent path is not a unique actual AXTable ancestry")
        }
        let scrolls = path.filter { $0.role == kAXScrollAreaRole }
        guard !scrolls.isEmpty, scrolls.count <= 8,
              scrolls.allSatisfy({ scroll in
                  guard let description = scroll.rawDescription else { return false }
                  return !description.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
              }) else {
            throw NativeRowBindingReadFailure.unavailable("actual AXScrollArea ancestry is missing a public description")
        }
        guard NativeRowBindingValidation.viewportFrameMatches(
            window: path[0].frame, scrollPath: scrolls.map(\.frame), expected: context.viewport_frame) else {
            throw NativeRowBindingReadFailure.unavailable("clipped AX viewport differs from Source")
        }
        return (path, scrolls)
    }

    private func actualGroup(in path: [NativeRowAncestorSnapshot]) -> String? {
        path.reversed().first(where: {
            ($0.role == kAXGroupRole || $0.role.hasSuffix("Group")) && !$0.normalizedLabel.isEmpty
        })?.normalizedLabel
    }

    private func frame(_ slots: [NativeRowBatchSlot], positionIndex: Int,
                       sizeIndex: Int) throws -> HitRect {
        let rawPosition = try value(slots, positionIndex, kAXPositionAttribute)
        let rawSize = try value(slots, sizeIndex, kAXSizeAttribute)
        guard CFGetTypeID(rawPosition) == AXValueGetTypeID(),
              CFGetTypeID(rawSize) == AXValueGetTypeID() else {
            throw NativeRowBindingReadFailure.unavailable("AX geometry is not a CGPoint/CGSize pair")
        }
        var point = CGPoint.zero
        var size = CGSize.zero
        guard AXValueGetValue(unsafeDowncast(rawPosition, to: AXValue.self), .cgPoint, &point),
              AXValueGetValue(unsafeDowncast(rawSize, to: AXValue.self), .cgSize, &size) else {
            throw NativeRowBindingReadFailure.unavailable("AX geometry value is malformed")
        }
        let result = HitRect(CGRect(origin: point, size: size))
        guard result.valid else { throw NativeRowBindingReadFailure.unavailable("AX geometry is invalid") }
        return result
    }

    private func readColumns(_ elements: [AXUIElement],
                             expected: [NativeDeviceContextRowColumn]) throws -> [NativeRowColumnSnapshot] {
        var result: [NativeRowColumnSnapshot] = []
        var seenIndices = Set<UInt8>()
        var seenElements: [AXUIElement] = []
        var seenHeaders: [AXUIElement] = []
        result.reserveCapacity(elements.count)
        for column in elements {
            let columnAttributes = try batch(column, [
                .required(kAXRoleAttribute), .required(kAXParentAttribute),
                .required(kAXWindowAttribute), .required(kAXIndexAttribute),
                .required(kAXHeaderAttribute), .required(kAXPositionAttribute),
                .required(kAXSizeAttribute)])
            let columnFrame = try frame(columnAttributes, positionIndex: 5, sizeIndex: 6)
            guard try text(columnAttributes, 0, kAXRoleAttribute) == kAXColumnRole,
                  CFEqual(try element(columnAttributes, 1, kAXParentAttribute), table),
                  CFEqual(try element(columnAttributes, 2, kAXWindowAttribute), window),
                  let rawIndex = NativeRowBindingValidation.rowAXIndex(try value(columnAttributes, 3, kAXIndexAttribute)),
                  rawIndex < UInt32(NativeRowBindingValidation.maximumColumns),
                  let index = UInt8(exactly: rawIndex) else {
                throw NativeRowBindingReadFailure.unavailable("actual AXColumn identity or parent is invalid")
            }
            guard seenIndices.insert(index).inserted,
                  !seenElements.contains(where: { CFEqual($0, column) }) else {
                throw NativeRowBindingReadFailure.unavailable("AXColumns contains a duplicate index or reference")
            }
            seenElements.append(column)
            let header = try element(columnAttributes, 4, kAXHeaderAttribute)
            let headerAttributes = try batch(header, [
                .required(kAXRoleAttribute), .required(kAXWindowAttribute),
                .optional(kAXTitleAttribute), .optional(kAXDescriptionAttribute),
                .optional(kAXValueAttribute)])
            let headerRole = try text(headerAttributes, 0, kAXRoleAttribute)
            let headerTitle = try optionalText(headerAttributes, 2, kAXTitleAttribute)
            let headerDescription = try optionalText(headerAttributes, 3, kAXDescriptionAttribute)
            let headerValue = try optionalStringValue(headerAttributes, 4, kAXValueAttribute)
            let label = headerTitle ?? headerDescription ?? headerValue
            guard [kAXStaticTextRole, kAXButtonRole].contains(headerRole),
                  CFEqual(try element(headerAttributes, 1, kAXWindowAttribute), window),
                  !seenHeaders.contains(where: { CFEqual($0, header) }),
                  let label, !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                  label.utf8.count <= 256,
                  !label.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else {
                throw NativeRowBindingReadFailure.unavailable("AXHeader reference, window, or label is invalid")
            }
            seenHeaders.append(header)
            result.append(NativeRowColumnSnapshot(element: column, axIndex: index,
                frame: columnFrame, header: header, headerLabel: label))
        }
        guard NativeRowBindingValidation.orderedColumnsMatch(result, expected: expected) else {
            throw NativeRowBindingReadFailure.unavailable("AXColumns order, header, or geometry differs from Source")
        }
        return result
    }

    private func cellValue(_ cell: AXUIElement) throws -> (NativeRowSelectorValue, AXUIElement?) {
        let cellValueAttributes = try batch(cell, [.optional(kAXValueAttribute)])
        if let raw = try optionalValue(cellValueAttributes, 0, kAXValueAttribute) {
            guard let value = NativeRowSelectorValue.fromAXValue(raw) else {
                throw NativeRowBindingReadFailure.unavailable("AXCell value type is unsupported or over budget")
            }
            return (value, nil)
        }
        guard let children = try elements(cell, kAXChildrenAttribute,
                limit: NativeRowBindingValidation.maximumValueChildren), children.count == 1,
              let child = children.first else {
            throw NativeRowBindingReadFailure.unavailable("AXCell has no unique privacy-safe StaticText value")
        }
        let childAttributes = try batch(child, [
            .required(kAXRoleAttribute), .optional(kAXSubroleAttribute),
            .required(kAXParentAttribute), .required(kAXWindowAttribute)])
        guard try text(childAttributes, 0, kAXRoleAttribute) == kAXStaticTextRole,
              try optionalText(childAttributes, 1, kAXSubroleAttribute) != "AXSecureTextField",
              CFEqual(try element(childAttributes, 2, kAXParentAttribute), cell),
              CFEqual(try element(childAttributes, 3, kAXWindowAttribute), window) else {
            throw NativeRowBindingReadFailure.unavailable("AXCell child is not a safe direct StaticText")
        }
        let childValueAttributes = try batch(child, [.required(kAXValueAttribute)])
        guard let value = NativeRowSelectorValue.fromAXValue(
            try value(childValueAttributes, 0, kAXValueAttribute)) else {
            throw NativeRowBindingReadFailure.unavailable("StaticText value type is unsupported or over budget")
        }
        return (value, child)
    }

    private func elements(_ parent: AXUIElement, _ name: String, limit: Int) throws -> [AXUIElement]? {
        try prepare(parent)
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(parent, name as CFString, &count)
        guard monotonicMS() < deadline else {
            throw NativeRowBindingReadFailure.unavailable("AX list count deadline exceeded for \(name)")
        }
        guard countError == .success else {
            throw NativeRowBindingReadFailure.unavailable("AX list count \(name) failed with AXError \(countError.rawValue)")
        }
        guard NativeRowBindingValidation.validBoundedListCount(count, limit: limit) else {
            throw NativeRowBindingReadFailure.unavailable("AX list count \(name) is outside its bound")
        }
        try prepare(parent)
        var raw: CFArray?
        let copyError = AXUIElementCopyAttributeValues(parent, name as CFString, 0, count, &raw)
        guard monotonicMS() < deadline else {
            throw NativeRowBindingReadFailure.unavailable("AX list copy deadline exceeded for \(name)")
        }
        guard copyError == .success else {
            guard raw == nil else {
                throw NativeRowBindingReadFailure.unavailable("AX list \(name) returned values with AXError \(copyError.rawValue)")
            }
            throw NativeRowBindingReadFailure.unavailable("AX list copy \(name) failed with AXError \(copyError.rawValue)")
        }
        guard let raw, CFGetTypeID(raw) == CFArrayGetTypeID(),
              CFArrayGetCount(raw) == count else {
            throw NativeRowBindingReadFailure.unavailable("AX list \(name) result shape differs from its bounded count")
        }
        let values = raw as [AnyObject]
        var result: [AXUIElement] = []
        result.reserveCapacity(values.count)
        for value in values {
            guard CFGetTypeID(value) == AXUIElementGetTypeID() else {
                throw NativeRowBindingReadFailure.unavailable("AX list contains a non-element value")
            }
            let element = unsafeDowncast(value, to: AXUIElement.self)
            try prepare(element)
            result.append(element)
        }
        return result
    }

    private func batch(_ element: AXUIElement,
                       _ attributes: [NativeRowBatchAttribute]) throws -> [NativeRowBatchSlot] {
        let names = NSArray(array: attributes.map { $0.name as NSString }) as CFArray
        try prepare(element)
        var values: CFArray?
        let error = AXUIElementCopyMultipleAttributeValues(element, names,
            AXCopyMultipleAttributeOptions(rawValue: 0), &values)
        guard monotonicMS() < deadline else {
            throw NativeRowBindingReadFailure.unavailable(
                "AX attribute batch deadline exceeded for \(attributes.map(\.name).joined(separator: ","))")
        }
        do {
            return try NativeRowBindingValidation.decodeBatchResponse(error: error,
                values: values, attributes: attributes)
        } catch NativeRowBatchDecodeFailure.invalid(let reason) {
            throw NativeRowBindingReadFailure.unavailable(reason)
        }
    }

    private func value(_ slots: [NativeRowBatchSlot], _ index: Int,
                       _ name: String) throws -> CFTypeRef {
        guard slots.indices.contains(index) else {
            throw NativeRowBindingReadFailure.unavailable("AX batch omitted requested attribute \(name)")
        }
        guard case .value(let raw) = slots[index] else {
            throw NativeRowBindingReadFailure.unavailable("required AX attribute \(name) is absent")
        }
        return raw
    }

    private func optionalValue(_ slots: [NativeRowBatchSlot], _ index: Int,
                               _ name: String) throws -> CFTypeRef? {
        guard slots.indices.contains(index) else {
            throw NativeRowBindingReadFailure.unavailable("AX batch omitted requested attribute \(name)")
        }
        switch slots[index] {
        case .value(let raw): return raw
        case .absent: return nil
        }
    }

    private func text(_ slots: [NativeRowBatchSlot], _ index: Int,
                      _ name: String) throws -> String {
        let raw = try value(slots, index, name)
        guard CFGetTypeID(raw) == CFStringGetTypeID(),
              let result = raw as? String, result.utf8.count <= 512 else {
            throw NativeRowBindingReadFailure.unavailable("AX attribute \(name) has invalid text type or size")
        }
        return result
    }

    private func optionalText(_ slots: [NativeRowBatchSlot], _ index: Int,
                              _ name: String) throws -> String? {
        guard let raw = try optionalValue(slots, index, name) else { return nil }
        guard CFGetTypeID(raw) == CFStringGetTypeID(),
              let result = raw as? String, result.utf8.count <= 512 else {
            throw NativeRowBindingReadFailure.unavailable("optional AX attribute \(name) has invalid text type or size")
        }
        return result
    }

    private func optionalStringValue(_ slots: [NativeRowBatchSlot], _ index: Int,
                                     _ name: String) throws -> String? {
        guard let raw = try optionalValue(slots, index, name) else { return nil }
        guard CFGetTypeID(raw) == CFStringGetTypeID(),
              let result = raw as? String, result.utf8.count <= 256 else { return nil }
        return result
    }

    private func element(_ slots: [NativeRowBatchSlot], _ index: Int,
                         _ name: String) throws -> AXUIElement {
        let raw = try value(slots, index, name)
        guard CFGetTypeID(raw) == AXUIElementGetTypeID() else {
            throw NativeRowBindingReadFailure.unavailable("AX relationship \(name) is not an AXUIElement")
        }
        let reference = unsafeDowncast(raw, to: AXUIElement.self)
        try prepare(reference)
        return reference
    }

    private func prepare(_ element: AXUIElement) throws {
        guard let timeoutMS = NativeRowBindingValidation.callTimeoutMilliseconds(
            now: monotonicMS(), deadline: deadline),
              AXUIElementSetMessagingTimeout(element, Float(timeoutMS) / 1000) == .success,
              monotonicMS() < deadline else {
            throw NativeRowBindingReadFailure.unavailable("AX call deadline or per-call timeout unavailable")
        }
        if verifiedProcessRefs.contains(where: { CFEqual($0, element) }) { return }
        var owner: pid_t = 0
        guard AXUIElementGetPid(element, &owner) == .success, monotonicMS() < deadline,
              owner == pid else {
            throw NativeRowBindingReadFailure.unavailable("AX reference belongs to another process or is unavailable")
        }
        verifiedProcessRefs.append(element)
    }

    private func readAXEnabled(_ element: AXUIElement) throws -> NativeAXEnabledObservation {
        try prepare(element)
        return readNativeAXEnabledObservation(element, deadline: deadline)
    }
}
